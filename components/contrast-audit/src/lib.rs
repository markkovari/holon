//! `contrast-audit` — a WCAG contrast auditor that runs on the lattice.
//!
//! Exports HTTP: `GET /` serves the page (which samples the screenshot's pixels
//! in the browser), `POST /audit` takes `{pairs:[{fg,bg,share}]}`, recomputes
//! every contrast ratio here (`wcag`), and asks Claude — reached by EGRESS, with
//! the API key from the vault — which failures to fix first and what to change
//! them to, returning `{report}`.
//!
//! Same two grants as `photo-critic`, an external secret and egress, and the
//! opposite payload. Where that one downscales an image so a smaller image can be
//! sent, this one MEASURES in the browser and sends no image at all — only the
//! colours found. A screenshot of an unreleased product is exactly the sort of
//! thing people paste into a contrast checker, and the interesting property here
//! is that it cannot leave the device even by accident.

#[allow(warnings)]
mod bindings;
mod wcag;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// How many pairs reach the model.
///
/// A quantised screenshot yields dozens of near-duplicate pairs and the tail is
/// all sampling noise. `wcag::audit` sorts worst-first, so this keeps the
/// findings and drops the ones that already pass comfortably.
const MAX_PAIRS: usize = 12;

/// The reply budget. A report is a few hundred tokens, so this is nearly all
/// headroom, deliberately.
///
/// `max_tokens` is a cap and not a reservation, so a large one costs nothing and
/// buys room for a model that thinks before it writes — `goalrun`'s
/// `--max-tokens` documents that case being real and expensive to diagnose:
/// `["thinking"]` and `stop_reason: max_tokens` at 4096 on claude-sonnet-5.
///
/// Honest history, because the comment here first claimed more than was measured:
/// this app asked for 1500, saw `the model returned no text` on a 200 ONCE, and
/// raising the budget was the change that followed. 1500 has since succeeded on
/// the same prompt and model, and `photo-critic` runs the same model at 1024, so
/// the budget was probably not the cause and the one failure is unexplained. The
/// error below is the part that earns its place: next time it will say which empty
/// it is instead of leaving the next person to guess as I did.
const MAX_TOKENS: u32 = 16000;

const PROMPT: &str = "You are an accessibility engineer reviewing the colour contrast of a user \
interface. You are given colour pairs sampled from a screenshot, with WCAG 2.1 contrast ratios that \
have ALREADY been computed and verified — treat them as ground truth and do NOT recompute them or \
dispute them. `share` is roughly how much of the sampled image that pair covers.\n\n\
Respond in GitHub-flavored markdown with exactly these sections and nothing else:\n\n\
## Verdict\nOne line: is this interface's contrast acceptable, borderline, or failing?\n\n\
## Fix first\nThe failing pairs that matter most, worst and most widespread first. For each: the \
pair, its ratio, and a CONCRETE replacement — give an actual hex colour that clears 4.5:1 against \
the same background while staying as close as possible to the original hue. State the ratio your \
suggested colour achieves.\n\n\
## Already fine\nOne short line listing the pairs that pass, so the reader knows what not to touch.\n\n\
## Caveats\nWhat this sampling cannot see: which pairs are actually text versus decoration, font \
sizes (3:1 is enough for large text), disabled controls, and colours behind images or gradients. Be \
brief and specific.\n\n\
IMPORTANT: these are sampled pixel colours, not a stylesheet, so a pair may be two adjacent \
decorative colours that no text ever sits on. Say so where it is likely rather than demanding a fix.";

fn model() -> String {
    config::get("contrast:model")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn api_key() -> Option<String> {
    match secrets::get("anthropic-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// A ceiling on a body read into memory, not a policy. Far smaller than
/// `photo-critic`'s: this endpoint takes a list of colours, so anything
/// approaching a megabyte is not a request this app has a use for.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// POST a JSON body to Anthropic and return (status, response-bytes).
fn post_anthropic(body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let _ = headers.set(&"content-length".to_string(), &[body.len().to_string().into_bytes()]);
    let _ = headers.set(&"connection".to_string(), &[b"close".to_vec()]);
    let _ = headers.set(&"anthropic-version".to_string(), &[b"2023-06-01".to_vec()]);
    if let Some(k) = api_key() {
        let _ = headers.set(&"x-api-key".to_string(), &[k.into_bytes()]);
    }
    let req = OutgoingRequest::new(headers);
    let e = |m: &str| m.to_string();
    req.set_method(&Method::Post).map_err(|_| e("method"))?;
    req.set_scheme(Some(&Scheme::Https)).map_err(|_| e("scheme"))?;
    req.set_authority(Some(&"api.anthropic.com".to_string())).map_err(|_| e("authority"))?;
    req.set_path_with_query(Some(&"/v1/messages".to_string())).map_err(|_| e("path"))?;
    {
        let out = req.body().map_err(|_| e("body"))?;
        {
            let stream = out.write().map_err(|_| e("write"))?;
            for chunk in body.chunks(4096) {
                stream.blocking_write_and_flush(chunk).map_err(|_| e("body write"))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| e("finish"))?;
    }
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(30_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(180_000_000_000));
    let _ = opts.set_between_bytes_timeout(Some(180_000_000_000));
    let fut = outgoing_handler::handle(req, Some(opts)).map_err(|err| format!("handle: {err:?}"))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| e("no response"))?
        .map_err(|_| e("taken"))?
        .map_err(|err| format!("http: {err:?}"))?;
    let status = resp.status();
    let mut buf = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        // The same ceiling on the way back — a reply far larger
                        // than the request that provoked it is a mistake or an
                        // attack, and either way not something to accumulate
                        // while deciding.
                        if buf.len() + c.len() > MAX_BODY_BYTES * 8 {
                            return Err(e("the response body is too large"));
                        }
                        buf.extend_from_slice(&c);
                    }
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok((status, buf))
}

/// Read the whole incoming request body.
fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(body) = request.consume() {
        if let Ok(stream) = body.stream() {
            loop {
                match stream.blocking_read(65536) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        // No error channel here, so an over-long body reads as
                        // EMPTY rather than as a plausible prefix of itself — a
                        // truncated pair list would audit some of an interface
                        // and report as though it had seen all of it.
                        if buf.len() + c.len() > MAX_BODY_BYTES {
                            return Vec::new();
                        }
                        buf.extend_from_slice(&c);
                    }
                    Err(StreamError::Closed) => break,
                    // A failed read is not an end of body: collapsing the two
                    // returns a truncated payload as if it were whole.
                    Err(_) => break,
                }
            }
        }
    }
    buf
}

/// Render the audited pairs as the table the model reads.
///
/// A table and not JSON: the numbers are already decided and this is the form a
/// reader of the transcript can check by eye against the report.
fn table(pairs: &[wcag::Pair]) -> String {
    let mut s = String::from("| text | background | ratio | grade | share |\n|---|---|--:|---|--:|\n");
    for p in pairs {
        s.push_str(&format!(
            "| `{}` | `{}` | {:.2}:1 | {} | {:.0}% |\n",
            p.fg,
            p.bg,
            p.ratio,
            p.verdict(),
            p.share * 100.0
        ));
    }
    s
}

/// The whole audit path: parse the pairs, do the maths, ask the model.
fn audit(request: &IncomingRequest) -> Result<String, String> {
    let body = read_body(request);
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("bad JSON body: {e}"))?;
    let claims: Vec<(String, String, f64)> = v["pairs"]
        .as_array()
        .ok_or("missing pairs")?
        .iter()
        .filter_map(|p| {
            Some((
                p["fg"].as_str()?.to_string(),
                p["bg"].as_str()?.to_string(),
                // Absent is not zero-weighted-out, it is simply unknown; a
                // missing share must not silently sort a pair last.
                p["share"].as_f64().unwrap_or(0.0),
            ))
        })
        .collect();
    if claims.is_empty() {
        return Err("no colour pairs in the request".into());
    }
    // EVERY ratio recomputed here. See `wcag`'s module docs for why.
    let mut pairs = wcag::audit(&claims);
    if pairs.is_empty() {
        return Err("none of those pairs were two different colours".into());
    }
    let dropped = pairs.len().saturating_sub(MAX_PAIRS);
    pairs.truncate(MAX_PAIRS);

    let failing = pairs.iter().filter(|p| !p.passes_aa()).count();
    let mut prompt = format!(
        "{PROMPT}\n\n{} pair(s) sampled, {failing} below 4.5:1.",
        pairs.len()
    );
    if dropped > 0 {
        // Said out loud, because a report that silently saw two thirds of the
        // evidence reads exactly like one that saw all of it.
        prompt.push_str(&format!(
            " {dropped} further pair(s) with better contrast were not included."
        ));
    }
    prompt.push_str("\n\n");
    prompt.push_str(&table(&pairs));

    let req_body = format!(
        "{{\"model\":{},\"max_tokens\":{MAX_TOKENS},\"messages\":[{{\"role\":\"user\",\"content\":{}}}]}}",
        json_str(&model()),
        json_str(&prompt)
    );
    let (status, resp) = post_anthropic(req_body.as_bytes())?;
    if !(200..300).contains(&status) {
        let snippet = String::from_utf8_lossy(&resp).chars().take(300).collect::<String>();
        return Err(format!("messages API {status}: {snippet}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&resp).map_err(|e| format!("bad response: {e}"))?;
    let text: String = parsed["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if !text.is_empty() {
        return Ok(text);
    }
    // Say WHICH empty this is. A thinking model that ran out of budget returns a
    // perfectly valid 200 whose content holds no text block, and "the model
    // returned no text" sent me looking at the parse rather than at `max_tokens`.
    let kinds: Vec<&str> = parsed["content"]
        .as_array()
        .map(|b| b.iter().filter_map(|b| b["type"].as_str()).collect())
        .unwrap_or_default();
    let stop = parsed["stop_reason"].as_str().unwrap_or("?");
    if stop == "max_tokens" {
        Err(format!(
            "the model spent all {MAX_TOKENS} tokens without writing an answer \
             (blocks: {kinds:?}) — raise MAX_TOKENS or use a smaller model"
        ))
    } else {
        Err(format!("the model returned no text (stop_reason {stop}, blocks: {kinds:?})"))
    }
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[ctype.as_bytes().to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(status);
    let out = resp.body().expect("body");
    ResponseOutparam::set(response_out, Ok(resp));
    if let Ok(stream) = out.write() {
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
        drop(stream);
    }
    let _ = OutgoingBody::finish(out, None);
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/");
        match (request.method(), route) {
            (Method::Get, "/") | (Method::Get, "/index.html") => {
                respond(response_out, 200, "text/html; charset=utf-8", PAGE.as_bytes());
            }
            (Method::Post, "/audit") => match audit(&request) {
                Ok(text) => {
                    let body = format!("{{\"report\":{}}}", json_str(&text));
                    respond(response_out, 200, "application/json", body.as_bytes());
                }
                Err(e) => {
                    let body = format!("{{\"error\":{}}}", json_str(&e));
                    respond(response_out, 500, "application/json", body.as_bytes());
                }
            },
            _ => respond(response_out, 404, "text/plain", b"not found"),
        }
    }
}

bindings::export!(Component with_types_in bindings);

const PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Contrast Audit</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.55 system-ui, sans-serif; max-width: 760px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.6rem; margin: 0 0 .25rem; }
  .sub { opacity: .65; margin: 0 0 1.5rem; }
  .drop { border: 2px dashed currentColor; border-radius: 14px; padding: 2.5rem 1rem; text-align: center;
          opacity: .8; cursor: pointer; transition: .15s; display:block; }
  .drop:hover, .drop.over { opacity: 1; background: rgba(127,127,127,.08); }
  input[type=file] { display: none; }
  #pairs { margin: 1rem 0; display: none; }
  #pairs table { border-collapse: collapse; width: 100%; font-size: .9rem; }
  #pairs th, #pairs td { text-align: left; padding: .3rem .5rem; border-bottom: 1px solid rgba(127,127,127,.2); }
  #pairs td.n { text-align: right; font-variant-numeric: tabular-nums; }
  .sw { display: inline-block; padding: .1rem .45rem; border-radius: 4px; font-family: ui-monospace, monospace; font-size: .85em; }
  .bad { color: #c0392b; font-weight: 600; }
  #out h2 { font-size: 1.15rem; margin: 1.4rem 0 .3rem; border-bottom: 1px solid rgba(127,127,127,.25); padding-bottom:.2rem; }
  #out li { margin: .2rem 0; }
  #out code { background: rgba(127,127,127,.15); padding: .05rem .3rem; border-radius: 4px; }
  #out table { border-collapse: collapse; margin: .4rem 0; }
  #out th, #out td { text-align: left; padding: .25rem .5rem; border-bottom: 1px solid rgba(127,127,127,.2); }
  .spin { display: none; opacity: .7; }
  .err { color: #c0392b; white-space: pre-wrap; }
</style></head>
<body>
  <h1>🎨 Contrast Audit</h1>
  <p class="sub">Drop a screenshot — get the colour pairs that fail WCAG, worst first, with a hex to replace them.
  <strong>The image never leaves your browser</strong>: it is measured here and only the colours are sent.</p>
  <label class="drop" id="drop"><input type="file" id="file" accept="image/*"><div>Drop a screenshot here, or click to choose</div></label>
  <div id="pairs"></div>
  <p class="spin" id="spin">Sampling pixels &amp; asking the auditor…</p>
  <div id="out"></div>
<script>
const drop=document.getElementById('drop'),file=document.getElementById('file'),out=document.getElementById('out'),spin=document.getElementById('spin'),pairsEl=document.getElementById('pairs');
['dragover','dragenter'].forEach(e=>drop.addEventListener(e,ev=>{ev.preventDefault();drop.classList.add('over');}));
['dragleave','drop'].forEach(e=>drop.addEventListener(e,ev=>{ev.preventDefault();drop.classList.remove('over');}));
drop.addEventListener('drop',ev=>{if(ev.dataTransfer.files[0])handle(ev.dataTransfer.files[0]);});
file.addEventListener('change',()=>{if(file.files[0])handle(file.files[0]);});
function md(t){const L=t.split('\n');let h='',inl=false,intbl=false;
 const inline=s=>s.replace(/\*\*(.+?)\*\*/g,'<strong>$1</strong>').replace(/`([^`]+)`/g,'<code>$1</code>');
 const closeAll=()=>{if(inl){h+='</ul>';inl=false;}if(intbl){h+='</table>';intbl=false;}};
 for(let ln of L){
  if(/^\s*\|/.test(ln)){ if(/^\s*\|[\s:|-]+\|\s*$/.test(ln))continue; if(!intbl){closeAll();h+='<table>';intbl=true;}
   const cells=ln.trim().replace(/^\||\|$/g,'').split('|').map(c=>inline(c.trim()));
   h+='<tr>'+cells.map(c=>'<td>'+c+'</td>').join('')+'</tr>'; continue; }
  if(ln.startsWith('## ')){closeAll();h+='<h2>'+inline(ln.slice(3))+'</h2>';}
  else if(/^\s*[-*]\s+/.test(ln)){if(intbl){h+='</table>';intbl=false;}if(!inl){h+='<ul>';inl=true;}h+='<li>'+inline(ln.replace(/^\s*[-*]\s+/,''))+'</li>';}
  else if(ln.trim()===''){closeAll();}
  else {closeAll();h+='<p>'+inline(ln)+'</p>';}}
 closeAll(); return h;}
function loadImage(f){return new Promise((res,rej)=>{const i=new Image();i.onload=()=>res(i);i.onerror=rej;i.src=URL.createObjectURL(f);});}
const hex=(r,g,b)=>'#'+[r,g,b].map(v=>v.toString(16).padStart(2,'0')).join('');
function lin(c){c/=255;return c<=0.03928?c/12.92:Math.pow((c+0.055)/1.055,2.4);}
const lum=([r,g,b])=>0.2126*lin(r)+0.7152*lin(g)+0.0722*lin(b);
function ratio(a,b){const la=lum(a),lb=lum(b),hi=Math.max(la,lb),lo=Math.min(la,lb);return (hi+0.05)/(lo+0.05);}
// Quantise to 5 bits per channel and count. A screenshot is mostly flat fills, so
// this collapses antialiasing into the colour it is a blend of, rather than
// reporting a hundred one-pixel shades.
function palette(img){
  const S=192, s=Math.min(1,S/Math.max(img.width,img.height));
  const w=Math.max(1,Math.round(img.width*s)), h=Math.max(1,Math.round(img.height*s));
  const c=document.createElement('canvas');c.width=w;c.height=h;
  const x=c.getContext('2d',{willReadFrequently:true});
  x.imageSmoothingEnabled=true;x.imageSmoothingQuality='high';x.drawImage(img,0,0,w,h);
  const d=x.getImageData(0,0,w,h).data, counts=new Map();
  let total=0;
  for(let i=0;i<d.length;i+=4){
    if(d[i+3]<128)continue;                       // transparent pixels are not a colour
    const q=[d[i]>>3<<3, d[i+1]>>3<<3, d[i+2]>>3<<3];
    const k=q.join(',');counts.set(k,(counts.get(k)||0)+1);total++;
  }
  if(!total)return [];
  return [...counts.entries()].sort((a,b)=>b[1]-a[1]).slice(0,6)
    .map(([k,n])=>({rgb:k.split(',').map(Number),share:n/total}));
}
// Every pair among the dominant colours. Which one is "text" is unknowable from
// pixels alone, and the component says so in its own caveats.
function pairsOf(pal){
  const out=[];
  for(let i=0;i<pal.length;i++)for(let j=i+1;j<pal.length;j++){
    const a=pal[i],b=pal[j];
    out.push({fg:hex(...a.rgb),bg:hex(...b.rgb),share:Math.min(a.share,b.share),ratio:ratio(a.rgb,b.rgb)});
  }
  return out.sort((p,q)=>p.ratio-q.ratio).slice(0,12);
}
function showPairs(ps){
  pairsEl.innerHTML='<table><tr><th>pair</th><th>ratio</th><th>AA</th></tr>'+ps.map(p=>
    '<tr><td><span class="sw" style="color:'+p.fg+';background:'+p.bg+'">'+p.fg+' on '+p.bg+'</span></td>'+
    '<td class="n">'+p.ratio.toFixed(2)+':1</td>'+
    '<td>'+(p.ratio>=4.5?'pass':'<span class="bad">fail</span>')+'</td></tr>').join('')+'</table>';
  pairsEl.style.display='block';
}
async function handle(f){
  out.innerHTML='';pairsEl.style.display='none';spin.style.display='block';
  try{
    const img=await loadImage(f);
    const pal=palette(img);
    if(!pal.length)throw new Error('no opaque pixels to sample');
    const ps=pairsOf(pal);
    showPairs(ps);                                 // drawn from the local maths
    // Only the colours go over the wire. No image, ever.
    const r=await fetch('/audit',{method:'POST',headers:{'content-type':'application/json'},
      body:JSON.stringify({pairs:ps.map(p=>({fg:p.fg,bg:p.bg,share:p.share}))})});
    const j=await r.json();
    out.innerHTML=r.ok?md(j.report):'<p class="err">'+(j.error||'error')+'</p>';
  }catch(e){out.innerHTML='<p class="err">'+e+'</p>';}
  finally{spin.style.display='none';}
}
</script>
</body></html>"##;
