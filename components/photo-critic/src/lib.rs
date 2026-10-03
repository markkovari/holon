//! `photo-critic` — a photo-critique app that runs on the lattice.
//!
//! Exports HTTP: `GET /` serves the upload page (which downscales the image in
//! the browser before upload), `POST /evaluate` takes `{media_type, data}` and
//! asks Claude's vision API — reached by EGRESS, with the API key from the vault
//! — for an honest critique, returning `{critique}`. Same egress + secret shape
//! as `anthropic-provider`, but serving HTTP and sending an image block.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../host/wit/deps/comp-secrets",
            "wit",
        ],
        world: "demo:photocritic/photo-critic",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::comp::secrets::reader as secrets;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{
    ErrorCode, Fields, Method, Request, RequestOptions, Response, Scheme,
};
use bindings::wasi::config::store as config;

guestio::guest_p3_respond!();

struct Component;

const DEFAULT_MODEL: &str = "claude-sonnet-5";

const PROMPT: &str = "You are a candid, expert photography critic — encouraging but honest, \
and always specific to THIS image, never generic. Evaluate the attached photo. Respond in \
GitHub-flavored markdown with exactly these sections and nothing else:\n\n\
## Interesting\nA score out of 10, then one honest sentence on what makes this photo \
interesting (or not).\n\n\
## Composition\nWhat works and what does not — framing, balance, leading lines, the subject, \
the background, and the light.\n\n\
## What to change\n3 to 5 concrete, actionable changes (reframe, crop, wait for different \
light, move the subject, etc.) that would make it a stronger photo.\n\n\
## Verdict\nOne punchy line.\n\n\
IMPORTANT: the image was downscaled and JPEG-compressed for upload, so do NOT \
comment on technical sharpness or call it 'soft' or 'out of focus' unless there \
is obvious motion blur or the subject is clearly missing focus — otherwise assume \
it is sharp and judge composition, light, subject, and moment instead.";

fn model() -> String {
    config::get("photo:model")
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

/// A ceiling on a body read into memory, not a policy: past this the read gives
/// up, rather than growing until the store's memory cap traps the component and
/// the connection simply closes. Both directions — a request, and a model's
/// reply to one.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// POST a JSON body to Anthropic and return (status, response-bytes).
async fn post_anthropic(body: Vec<u8>) -> Result<(u16, Vec<u8>), String> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("content-length", &[body.len().to_string().into_bytes()]);
    let _ = headers.set("connection", &[b"close".to_vec()]);
    let _ = headers.set("anthropic-version", &[b"2023-06-01".to_vec()]);
    if let Some(k) = api_key() {
        let _ = headers.set("x-api-key", &[k.into_bytes()]);
    }
    let e = |m: &str| m.to_string();
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(30_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(180_000_000_000));
    let _ = opts.set_between_bytes_timeout(Some(180_000_000_000));
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (mut tx, rx) = bindings::wit_stream::new();
    wit_bindgen::spawn_local(async move {
        tx.write_all(body).await;
    });
    let (req, _sent) = Request::new(headers, Some(rx), trailers_rx, Some(opts));
    req.set_method(&Method::Post).map_err(|_| e("method"))?;
    req.set_scheme(Some(&Scheme::Https)).map_err(|_| e("scheme"))?;
    req.set_authority(Some("api.anthropic.com")).map_err(|_| e("authority"))?;
    req.set_path_with_query(Some("/v1/messages")).map_err(|_| e("path"))?;
    let resp =
        bindings::p3::http::client::send(req).await.map_err(|err| format!("http: {err:?}"))?;
    let status = resp.get_status_code();
    let (res_tx, res_rx) = bindings::wit_future::new(|| Ok(()));
    let (mut stream, trailers) = Response::consume_body(resp, res_rx);
    let mut buf = Vec::new();
    loop {
        buf.reserve(8192);
        let (st, b) = stream.read(buf).await;
        buf = b;
        // The same ceiling on the way back. This is a model's answer over a
        // network we do not control, and a reply larger than the request that
        // provoked it is either a mistake or an attack — either way not
        // something to hold in memory while deciding.
        if buf.len() > MAX_BODY_BYTES {
            return Err(e("the response body is too large"));
        }
        match st {
            wit_bindgen::StreamResult::Complete(_) => {}
            wit_bindgen::StreamResult::Dropped | wit_bindgen::StreamResult::Cancelled => break,
        }
    }
    drop(res_tx);
    // Like the p2 loop's `Err(_) => break`: what arrived is returned, and a
    // truncated reply fails loudly as bad JSON in `evaluate`.
    let _ = trailers.await;
    Ok((status, buf))
}

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

/// The whole evaluate path: parse the upload, ask the model, return the critique.
async fn evaluate(request: Request) -> Result<String, String> {
    // A failed or over-long read is EMPTY rather than a plausible prefix of
    // itself — a truncated image would decode into a critique of half a photograph.
    let body = read_body(request).await.unwrap_or_default();
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("bad JSON body: {e}"))?;
    let media_type = v["media_type"].as_str().ok_or("missing media_type")?;
    let data = v["data"].as_str().ok_or("missing data")?;
    if data.is_empty() {
        return Err("empty image".into());
    }
    // Build the vision request. `data` is base64 (safe inside quotes as-is).
    let req_body = format!(
        "{{\"model\":{},\"max_tokens\":1024,\"messages\":[{{\"role\":\"user\",\"content\":[\
         {{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":{},\"data\":\"{}\"}}}},\
         {{\"type\":\"text\",\"text\":{}}}]}}]}}",
        json_str(&model()), json_str(media_type), data, json_str(PROMPT)
    );
    let (status, resp) = post_anthropic(req_body.into_bytes()).await?;
    if !(200..300).contains(&status) {
        let snippet = String::from_utf8_lossy(&resp).chars().take(300).collect::<String>();
        return Err(format!("vision API {status}: {snippet}"));
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
    if text.is_empty() {
        Err("the model returned no text".into())
    } else {
        Ok(text)
    }
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/");
        match (request.get_method(), route) {
            (Method::Get, "/") | (Method::Get, "/index.html") => {
                respond(200, "text/html; charset=utf-8", PAGE)
            }
            (Method::Post, "/evaluate") => match evaluate(request).await {
                Ok(text) => {
                    let body = format!("{{\"critique\":{}}}", json_str(&text));
                    respond(200, "application/json", body)
                }
                Err(e) => {
                    let body = format!("{{\"error\":{}}}", json_str(&e));
                    respond(500, "application/json", body)
                }
            },
            _ => respond(404, "text/plain", "not found"),
        }
    }
}

bindings::export!(Component with_types_in bindings);

const PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Photo Critic</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.55 system-ui, sans-serif; max-width: 760px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.6rem; margin: 0 0 .25rem; }
  .sub { opacity: .65; margin: 0 0 1.5rem; }
  .drop { border: 2px dashed currentColor; border-radius: 14px; padding: 2.5rem 1rem; text-align: center;
          opacity: .8; cursor: pointer; transition: .15s; display:block; }
  .drop:hover, .drop.over { opacity: 1; background: rgba(127,127,127,.08); }
  input[type=file] { display: none; }
  img#preview { max-width: 100%; border-radius: 12px; margin: 1rem 0; display: none; }
  #out h2 { font-size: 1.15rem; margin: 1.4rem 0 .3rem; border-bottom: 1px solid rgba(127,127,127,.25); padding-bottom:.2rem; }
  #out li { margin: .2rem 0; }
  .spin { display: none; opacity: .7; }
  .err { color: #c0392b; white-space: pre-wrap; }
</style></head>
<body>
  <h1>📷 Photo Critic</h1>
  <p class="sub">Upload a photo — get an honest read on what's interesting, the composition, and what to change. It's downscaled in your browser before it's sent.</p>
  <label class="drop" id="drop"><input type="file" id="file" accept="image/*"><div>Drop a photo here, or click to choose</div></label>
  <img id="preview">
  <p class="spin" id="spin">Downscaling &amp; asking the critic…</p>
  <div id="out"></div>
<script>
const drop=document.getElementById('drop'),file=document.getElementById('file'),preview=document.getElementById('preview'),out=document.getElementById('out'),spin=document.getElementById('spin');
['dragover','dragenter'].forEach(e=>drop.addEventListener(e,ev=>{ev.preventDefault();drop.classList.add('over');}));
['dragleave','drop'].forEach(e=>drop.addEventListener(e,ev=>{ev.preventDefault();drop.classList.remove('over');}));
drop.addEventListener('drop',ev=>{if(ev.dataTransfer.files[0])handle(ev.dataTransfer.files[0]);});
file.addEventListener('change',()=>{if(file.files[0])handle(file.files[0]);});
function md(t){const L=t.split('\n');let h='',inl=false;for(let ln of L){ln=ln.replace(/\*\*(.+?)\*\*/g,'<strong>$1</strong>');if(ln.startsWith('## ')){if(inl){h+='</ul>';inl=false;}h+='<h2>'+ln.slice(3)+'</h2>';}else if(/^\s*[-*]\s+/.test(ln)){if(!inl){h+='<ul>';inl=true;}h+='<li>'+ln.replace(/^\s*[-*]\s+/,'')+'</li>';}else if(ln.trim()===''){if(inl){h+='</ul>';inl=false;}}else{if(inl){h+='</ul>';inl=false;}h+='<p>'+ln+'</p>';}}if(inl)h+='</ul>';return h;}
function loadImage(f){return new Promise((res,rej)=>{const i=new Image();i.onload=()=>res(i);i.onerror=rej;i.src=URL.createObjectURL(f);});}
function newCanvas(w,h){const c=document.createElement('canvas');c.width=w;c.height=h;const x=c.getContext('2d');x.imageSmoothingEnabled=true;x.imageSmoothingQuality='high';return[c,x];}
function downscale(img,LIMIT){
  // Step-wise halving with high-quality resampling keeps detail crisp; a single
  // big bilinear step smears it and reads as "soft/out of focus".
  let [c,x]=newCanvas(img.width,img.height); x.drawImage(img,0,0);
  let w=img.width,h=img.height;
  while(Math.max(w,h)>LIMIT*2){
    const nw=Math.round(w/2),nh=Math.round(h/2);
    const [c2,x2]=newCanvas(nw,nh); x2.drawImage(c,0,0,nw,nh); c=c2;x=x2;w=nw;h=nh;
  }
  const s=Math.min(1,LIMIT/Math.max(w,h)), fw=Math.round(w*s), fh=Math.round(h*s);
  const [fin,fx]=newCanvas(fw,fh); fx.drawImage(c,0,0,fw,fh);
  return fin.toDataURL('image/jpeg',0.92);
}
async function handle(f){
  out.innerHTML='';spin.style.display='block';
  try{
    const img=await loadImage(f);
    const url=downscale(img,1568);             // Anthropic's cap, high-quality resample
    preview.src=url;preview.style.display='block';
    const m=/^data:(.+?);base64,(.*)$/.exec(url);
    const r=await fetch('/evaluate',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({media_type:m[1],data:m[2]})});
    const j=await r.json();
    out.innerHTML=r.ok?md(j.critique):'<p class="err">'+(j.error||'error')+'</p>';
  }catch(e){out.innerHTML='<p class="err">'+e+'</p>';}
  finally{spin.style.display='none';}
}
</script>
</body></html>"##;
