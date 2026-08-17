//! `poll-domain` — a live poll, as one component.
//!
//! `GET /` creates one, `GET /p/{code}` votes in one, and the results are an SVG
//! the page embeds rather than a chart the page draws.
//!
//! ## What is imported and why that is the interesting part
//!
//! The chart comes from `svg:chart` and the QR from `qr:encode`, both already in
//! this repository, both pure compute. So there is no charting library in the page,
//! no QR library, no build step, and the same `<svg>` works in a browser, an email,
//! or a screenshot. `records:store` holds the polls and the votes; `id:generate`
//! mints the share code and the voter id.
//!
//! ## One vote per browser is a cookie, and that is why this has a browser suite
//!
//! A voter is a cookie this component sets — `voter=<ulid>`, `HttpOnly`, `SameSite`
//! — and a vote record keyed by `(poll, voter)`. Nothing about that is visible to an
//! API test driving one HTTP client: it would either pass a cookie it set itself or
//! never send one at all. Two browser contexts have two cookie jars, which is the
//! thing being asserted, so the e2e is Playwright rather than curl.
//!
//! Deliberately not identity: a cookie is clearable and this is a poll, not a
//! ballot. `auth-guard` is the component for the case where it matters, and saying
//! so here is cheaper than pretending a cookie is more than it is.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::id::generate::generator as ids;
use bindings::qr::encode::encoder as qr;
use bindings::records::store::store as records;
use bindings::svg::chart::charts as chart;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

struct Component;

const POLLS: &str = "polls";
const VOTES: &str = "votes";
/// How many options a poll may carry. A ballot with forty options is a form, not a
/// poll, and the chart stops being readable long before that.
const MAX_OPTIONS: usize = 8;
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that rather
/// than returning an error: the component dies mid-response and the caller sees
/// `connection closed before message completed`, three layers from the cause. The
/// page below is larger than that, so this is not optional.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

/// What a handler answers with. `set_voter` asks the router for a `set-cookie`.
struct Reply {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    set_voter: Option<String>,
}

impl Reply {
    fn json(status: u16, v: Value) -> Self {
        Reply {
            status,
            body: v.to_string().into_bytes(),
            content_type: "application/json",
            set_voter: None,
        }
    }
    fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    fn html(body: &str) -> Self {
        Reply {
            status: 200,
            body: body.as_bytes().to_vec(),
            content_type: "text/html; charset=utf-8",
            set_voter: None,
        }
    }
    /// An SVG document, sent as one. `image/svg+xml` and not JSON: the page embeds
    /// this with `<img>` and `innerHTML`, and a JSON-wrapped SVG is a string a
    /// browser will not render.
    fn svg(body: String) -> Self {
        Reply {
            status: 200,
            body: body.into_bytes(),
            content_type: "image/svg+xml",
            set_voter: None,
        }
    }
    fn with_voter(mut self, voter: &str) -> Self {
        self.set_voter = Some(voter.to_string());
        self
    }
}

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(16 * 1024) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => {
                // An over-long body reads as EMPTY rather than as a plausible prefix
                // of itself: half a JSON document can parse into something wrong.
                if out.len() + c.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&c);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `voter` cookie, or empty.
///
/// Parsed rather than matched: `voter=x` and `other=1; voter=x` and `xvoter=y` all
/// contain the substring, and only two of them are this cookie.
fn voter_of(request: &IncomingRequest) -> String {
    let headers = request.headers();
    for raw in headers.get(&"cookie".to_string()) {
        let s = String::from_utf8_lossy(&raw).into_owned();
        for pair in s.split(';') {
            if let Some((k, v)) = pair.split_once('=') {
                if k.trim() == "voter" {
                    return v.trim().to_string();
                }
            }
        }
    }
    String::new()
}

fn poll_by_code(code: &str) -> Option<(String, Value)> {
    // `code` is indexed, so this reads the one poll rather than every poll.
    let found = records::find_by(POLLS, "code", &json_value(code)).ok()?;
    let e = found.into_iter().next()?;
    let v: Value = serde_json::from_str(&e.data).ok()?;
    Some((e.id, v))
}

/// The form `find_by` wants: the JSON ENCODING of the value, not the value.
///
/// `record-store` indexes `serde_json` output, so a string field `AB12` is indexed
/// under `"AB12"` — quotes included — and `find_by(.., "AB12")` matches nothing while
/// returning `Ok(vec![])`. An empty result and a wrong query are indistinguishable
/// from here, which is why this is a named function rather than an inline `format!`.
fn json_value(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn counts_of(poll_id: &str, options: &[String]) -> Vec<u32> {
    let votes = records::find_by(VOTES, "poll", &json_value(poll_id)).unwrap_or_default();
    let mut counts = vec![0u32; options.len()];
    for v in &votes {
        let Ok(d) = serde_json::from_str::<Value>(&v.data) else { continue };
        if let Some(choice) = d["option"].as_str() {
            if let Some(i) = options.iter().position(|o| o == choice) {
                counts[i] += 1;
            }
        }
    }
    counts
}

fn options_of(poll: &Value) -> Vec<String> {
    poll["options"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn create_poll(body: &str) -> Reply {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return Reply::err(400, "invalid") };
    let question = v["question"].as_str().unwrap_or("").trim().to_string();
    let options: Vec<String> = v["options"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| o.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // Two options is the smallest thing that is a question. One is an announcement.
    if question.is_empty() || options.len() < 2 || options.len() > MAX_OPTIONS {
        return Reply::err(400, "invalid");
    }
    // A short code, because this goes in a URL a person reads out loud.
    let code = ids::short_code(6).to_uppercase();
    let data = json!({ "question": question, "options": options, "code": code });
    match records::create(POLLS, &data.to_string(), &["code".to_string()]) {
        Ok(e) => Reply::json(
            201,
            json!({ "id": e.id, "code": code, "question": question, "options": options }),
        ),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn results(code: &str) -> Reply {
    let Some((id, poll)) = poll_by_code(code) else { return Reply::err(404, "not_found") };
    let options = options_of(&poll);
    let counts = counts_of(&id, &options);
    Reply::json(
        200,
        json!({
            "code": code,
            "question": poll["question"],
            "options": options,
            "counts": counts,
            "total": counts.iter().sum::<u32>(),
        }),
    )
}

fn vote(code: &str, body: &str, voter: &str) -> Reply {
    let Some((id, poll)) = poll_by_code(code) else { return Reply::err(404, "not_found") };
    let Ok(v) = serde_json::from_str::<Value>(body) else { return Reply::err(400, "invalid") };
    let choice = v["option"].as_str().unwrap_or("").to_string();
    let options = options_of(&poll);
    if !options.contains(&choice) {
        return Reply::err(400, "invalid");
    }
    // A voter with no cookie yet gets one; the id is minted before the vote so the
    // record and the cookie cannot disagree.
    let (voter_id, fresh) = if voter.is_empty() {
        (ids::ulid(), true)
    } else {
        (voter.to_string(), false)
    };

    // One vote per browser. Checked by reading this poll's votes rather than a
    // compound index, because `(poll, voter)` is not a field the store indexes and a
    // poll's vote count is small by construction.
    let existing = records::find_by(VOTES, "poll", &json_value(&id)).unwrap_or_default();
    let already = existing.iter().any(|e| {
        serde_json::from_str::<Value>(&e.data)
            .map(|d| d["voter"].as_str() == Some(voter_id.as_str()))
            .unwrap_or(false)
    });
    if already {
        let counts = counts_of(&id, &options);
        // 409 and the counts, not 409 alone: the page has to show the result to
        // someone who has already voted, and a second request for it would be a
        // second round trip to say the same thing.
        return Reply::json(
            409,
            json!({ "error": "already_voted", "options": options, "counts": counts,
                    "total": counts.iter().sum::<u32>() }),
        );
    }

    let rec = json!({ "poll": id, "voter": voter_id, "option": choice });
    if records::create(VOTES, &rec.to_string(), &["poll".to_string()]).is_err() {
        return Reply::err(500, "store_error");
    }
    let counts = counts_of(&id, &options);
    let reply = Reply::json(
        200,
        json!({ "options": options, "counts": counts, "total": counts.iter().sum::<u32>(),
                "chose": choice }),
    );
    if fresh {
        reply.with_voter(&voter_id)
    } else {
        reply
    }
}

/// The results as an SVG document, drawn by `svg:chart`.
fn chart_svg(code: &str) -> Reply {
    let Some((id, poll)) = poll_by_code(code) else { return Reply::err(404, "not_found") };
    let options = options_of(&poll);
    let counts = counts_of(&id, &options);
    let data: Vec<chart::Slice> = options
        .iter()
        .zip(counts.iter())
        .map(|(label, n)| chart::Slice {
            label: label.clone(),
            value: *n as f64,
            // Empty means the renderer's palette, which is the point of having one.
            color: String::new(),
        })
        .collect();
    Reply::svg(chart::render(&chart::Chart {
        kind: chart::Kind::Bar,
        title: poll["question"].as_str().unwrap_or("").to_string(),
        data,
        width: 520,
        height: 260,
    }))
}

/// The share link as a QR, drawn by `qr:encode`.
fn qr_svg(code: &str, base: &str) -> Reply {
    if poll_by_code(code).is_none() {
        return Reply::err(404, "not_found");
    }
    match qr::svg(&format!("{base}/p/{code}"), qr::Ecc::Medium, 2) {
        Ok(svg) => Reply::svg(svg),
        Err(_) => Reply::err(500, "qr_failed"),
    }
}

/// Where this component is reachable, for the QR to point at.
///
/// From the request's own `host` header rather than config: the app is served
/// through an ingress, a proxy, or a tailnet name, and a hardcoded base URL produces
/// a QR that works on the machine that generated it and nowhere else.
fn base_url(request: &IncomingRequest) -> String {
    let host = request
        .headers()
        .get(&"host".to_string())
        .into_iter()
        .next()
        .map(|h| String::from_utf8_lossy(&h).into_owned())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let scheme = if host.ends_with(".ts.net") || host.ends_with(".test") {
        "https"
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.split('/').filter(|s| !s.is_empty()).collect();
        let method = request.method();
        let voter = voter_of(&request);
        let base = base_url(&request);
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        let reply = match (&method, seg.as_slice()) {
            (Method::Get, []) | (Method::Get, ["index.html"]) => Reply::html(CREATE_PAGE),
            (Method::Get, ["health"]) => Reply::json(200, json!({ "ok": true })),
            // The voting page. One HTML document for any code; it reads the code out
            // of its own URL, so there is no template to render server-side.
            (Method::Get, ["p", _code]) => Reply::html(VOTE_PAGE),
            (Method::Post, ["api", "polls"]) => create_poll(&body),
            (Method::Get, ["api", "polls", code]) => results(code),
            (Method::Post, ["api", "polls", code, "vote"]) => vote(code, &body, &voter),
            (Method::Get, ["api", "polls", code, "chart.svg"]) => chart_svg(code),
            (Method::Get, ["api", "polls", code, "qr.svg"]) => qr_svg(code, &base),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[reply.content_type.as_bytes().to_vec()]);
        if let Some(v) = &reply.set_voter {
            // HttpOnly so a script cannot read or forge it; SameSite=Lax so the
            // shared link still carries it; no Secure, because this serves over
            // plain http on a laptop and a Secure cookie would simply be dropped
            // there — the deployment behind TLS is where that flag belongs.
            let cookie = format!("voter={v}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000");
            let _ = headers.set(&"set-cookie".to_string(), &[cookie.into_bytes()]);
        }
        // No caching: a result that updates is the whole app, and a proxy that
        // remembers the first answer makes it look broken rather than stale.
        let _ = headers.set(&"cache-control".to_string(), &[b"no-store".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(reply.status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, &reply.body);
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);

const CREATE_PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Poll</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.55 system-ui, sans-serif; max-width: 640px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.6rem; margin: 0 0 .25rem; }
  .sub { opacity: .65; margin: 0 0 1.5rem; }
  label { display: block; margin: .9rem 0 .2rem; font-weight: 600; }
  input { width: 100%; padding: .5rem .6rem; font: inherit; border-radius: 8px;
          border: 1px solid rgba(127,127,127,.5); background: transparent; color: inherit; }
  button { margin-top: 1.2rem; padding: .55rem 1.1rem; font: inherit; font-weight: 600;
           border-radius: 8px; border: 1px solid currentColor; background: transparent;
           color: inherit; cursor: pointer; }
  button:hover { background: rgba(127,127,127,.12); }
  #made { display: none; margin-top: 1.8rem; }
  #link { font-family: ui-monospace, monospace; word-break: break-all; }
  .err { color: #c0392b; }
  img { display: block; margin: 1rem 0; }
</style></head>
<body>
  <h1>🗳 New poll</h1>
  <p class="sub">Ask something, share the link. One vote per browser.</p>
  <label for="q">Question</label>
  <input id="q" placeholder="Which one?">
  <label for="opts">Options, comma separated</label>
  <input id="opts" placeholder="Rust, Go, Zig">
  <button id="make">Create poll</button>
  <p class="err" id="err"></p>
  <div id="made">
    <h2>Share it</h2>
    <p id="link"></p>
    <img id="qr" width="180" height="180" alt="QR code for the poll link">
    <p><a id="open" href="#">Open the poll →</a></p>
  </div>
<script>
const $=id=>document.getElementById(id);
$('make').addEventListener('click', async () => {
  $('err').textContent='';
  const question=$('q').value.trim();
  const options=$('opts').value.split(',').map(s=>s.trim()).filter(Boolean);
  const r=await fetch('/api/polls',{method:'POST',headers:{'content-type':'application/json'},
    body:JSON.stringify({question,options})});
  const j=await r.json();
  if(!r.ok){ $('err').textContent = j.error==='invalid'
      ? 'A question and at least two options, please.' : ('error: '+j.error); return; }
  const url=location.origin+'/p/'+j.code;
  $('link').textContent=url;
  $('open').href='/p/'+j.code;
  $('qr').src='/api/polls/'+j.code+'/qr.svg';
  $('made').style.display='block';
  $('made').dataset.code=j.code;
});
</script>
</body></html>"##;

const VOTE_PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Vote</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.55 system-ui, sans-serif; max-width: 640px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.5rem; margin: 0 0 1rem; }
  button.opt { display: block; width: 100%; text-align: left; margin: .4rem 0; padding: .6rem .8rem;
        font: inherit; border-radius: 8px; border: 1px solid currentColor; background: transparent;
        color: inherit; cursor: pointer; }
  button.opt:hover { background: rgba(127,127,127,.12); }
  #note { opacity: .75; margin-top: 1rem; }
  #chart svg { max-width: 100%; height: auto; }
  .err { color: #c0392b; }
</style></head>
<body>
  <h1 id="q">…</h1>
  <div id="opts"></div>
  <p id="note"></p>
  <div id="chart"></div>
  <p class="err" id="err"></p>
<script>
const $=id=>document.getElementById(id);
const code=location.pathname.split('/').filter(Boolean)[1];
let voted=false;

async function load(){
  const r=await fetch('/api/polls/'+code);
  if(!r.ok){ $('q').textContent='No such poll.'; return; }
  const j=await r.json();
  $('q').textContent=j.question;
  $('opts').innerHTML='';
  j.options.forEach(o=>{
    const b=document.createElement('button');
    b.className='opt'; b.textContent=o; b.dataset.option=o;
    b.addEventListener('click',()=>cast(o));
    $('opts').appendChild(b);
  });
  show(j);
}

// The chart is fetched, not drawn: `svg:chart` renders it server-side and this
// drops the document straight in. No charting library, no build step.
async function show(j){
  $('note').textContent = j.total===1 ? '1 vote so far' : j.total+' votes so far';
  const r=await fetch('/api/polls/'+code+'/chart.svg');
  $('chart').innerHTML = r.ok ? await r.text() : '';
  $('chart').dataset.total=j.total;
}

async function cast(option){
  $('err').textContent='';
  const r=await fetch('/api/polls/'+code+'/vote',{method:'POST',
    headers:{'content-type':'application/json'},body:JSON.stringify({option})});
  const j=await r.json();
  if(r.status===409){
    voted=true;
    $('note').dataset.state='already';
    $('err').textContent='You have already voted in this poll.';
    show(j); return;
  }
  if(!r.ok){ $('err').textContent='error: '+j.error; return; }
  voted=true;
  $('note').dataset.state='voted';
  $('opts').dataset.chose=j.chose;
  show(j);
}
load();
</script>
</body></html>"##;
