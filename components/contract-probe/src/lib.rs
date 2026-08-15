//! `contract-probe` — the door onto `contract:registry` (see wit/probe.wit).
//!
//!   POST /publish                                   body is the contract
//!   GET  /current
//!   GET  /get?version=
//!   GET  /proposed?part=
//!   POST /ask?from=&to=&subject=&at=                body is what is being asked for
//!   GET  /pending?part=
//!   POST /answer?id=&verdict=granted|denied|counter body is the amendment or the reason
//!   POST /ratify?version=&part=&score=
//!   POST /built-against?candidate=&part=&version=
//!   GET  /composable?candidates=a,b
//!
//! Every route answers JSON, and a refusal is `{"error":"refused","detail":"…"}`
//! with a **200** — the same choice `graph-probe` and `memory-probe` made, for the
//! same reason: what is under test is what the registry decided, and a status code
//! would flatten "this part does not own that version" into the same shape as "the
//! host refused the link".

#[allow(warnings)]
mod bindings;

use bindings::contract::registry::registry as reg;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent(v))
        .unwrap_or_default()
}

/// Subjects are sentences and contracts are JSON, so both have to survive a query
/// string and a body.
fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn num(query: &str, key: &str) -> u32 {
    param(query, key).parse().unwrap_or(0)
}

fn signed(query: &str, key: &str) -> i32 {
    param(query, key).parse().unwrap_or(0)
}

fn verdict_of(name: &str) -> reg::Verdict {
    match name {
        "granted" => reg::Verdict::Granted,
        "counter" => reg::Verdict::Counter,
        // Anything unspelled is a refusal, never a grant — the same rule the
        // registry applies when it reads a verdict back.
        _ => reg::Verdict::Denied,
    }
}

fn verdict_name(v: reg::Verdict) -> &'static str {
    match v {
        reg::Verdict::Granted => "granted",
        reg::Verdict::Denied => "denied",
        reg::Verdict::Counter => "counter",
    }
}

fn err(e: reg::RegistryError) -> String {
    let (kind, msg) = match e {
        reg::RegistryError::Rejected(m) => ("rejected", m),
        reg::RegistryError::Unavailable(m) => ("unavailable", m),
        reg::RegistryError::Refused(m) => ("refused", m),
    };
    format!("{{\"error\":\"{kind}\",\"detail\":\"{}\"}}", esc(&msg))
}

fn contract_json(c: &reg::Contract) -> String {
    format!(
        "{{\"version\":{},\"body\":\"{}\",\"canonical\":{},\"owner\":\"{}\",\"from_request\":\"{}\"}}",
        c.version,
        esc(&c.body),
        c.canonical,
        esc(&c.owner),
        esc(&c.from_request)
    )
}

fn request_json(r: &reg::Request) -> String {
    format!(
        "{{\"id\":\"{}\",\"from_part\":\"{}\",\"to_part\":\"{}\",\"subject\":\"{}\",\"body\":\"{}\",\
         \"at_version\":{},\"answered\":{},\"verdict\":\"{}\",\"answer\":\"{}\"}}",
        esc(&r.id),
        esc(&r.from_part),
        esc(&r.to_part),
        esc(&r.subject),
        esc(&r.body),
        r.at_version,
        r.answered,
        verdict_name(r.verdict),
        esc(&r.answer)
    )
}

fn read_body(request: IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    while let Ok(chunk) = stream.blocking_read(64 * 1024) {
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.method();

        let body = match (&method, route.as_str()) {
            (Method::Post, "/publish") => match reg::publish(&read_body(request)) {
                Ok(v) => format!("{{\"version\":{v}}}"),
                Err(e) => err(e),
            },

            (Method::Get, "/current") => match reg::current() {
                Ok(c) => contract_json(&c),
                Err(e) => err(e),
            },

            (Method::Get, "/get") => match reg::get(num(&query, "version")) {
                Ok(Some(c)) => contract_json(&c),
                // Absence is an answer.
                Ok(None) => "{\"found\":false}".to_string(),
                Err(e) => err(e),
            },

            (Method::Get, "/proposed") => match reg::proposed(&param(&query, "part")) {
                Ok(Some(c)) => contract_json(&c),
                Ok(None) => "{\"found\":false}".to_string(),
                Err(e) => err(e),
            },

            (Method::Post, "/ask") => {
                let body = read_body(request);
                match reg::ask(
                    &param(&query, "from"),
                    &param(&query, "to"),
                    &param(&query, "subject"),
                    &body,
                    num(&query, "at"),
                ) {
                    Ok(id) => format!("{{\"id\":\"{}\"}}", esc(&id)),
                    Err(e) => err(e),
                }
            }

            (Method::Get, "/pending") => match reg::pending(&param(&query, "part")) {
                Ok(rs) => format!(
                    "{{\"requests\":[{}]}}",
                    rs.iter().map(request_json).collect::<Vec<_>>().join(",")
                ),
                Err(e) => err(e),
            },

            (Method::Post, "/answer") => {
                let v = verdict_of(&param(&query, "verdict"));
                let body = read_body(request);
                match reg::answer(&param(&query, "id"), v, &body) {
                    // 0 means no new version: a denial and a counter change
                    // nothing about what the parts build against.
                    Ok(version) => format!("{{\"version\":{version}}}"),
                    Err(e) => err(e),
                }
            }

            (Method::Post, "/ratify") => match reg::ratify(
                num(&query, "version"),
                &param(&query, "part"),
                signed(&query, "score"),
            ) {
                Ok(()) => "{\"ok\":true}".to_string(),
                Err(e) => err(e),
            },

            (Method::Post, "/built-against") => match reg::built_against(
                &param(&query, "candidate"),
                &param(&query, "part"),
                num(&query, "version"),
            ) {
                Ok(()) => "{\"ok\":true}".to_string(),
                Err(e) => err(e),
            },

            (Method::Get, "/composable") => {
                let candidates: Vec<String> = param(&query, "candidates")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                match reg::composable(&candidates) {
                    // An empty list is the yes. Reported as a list rather than a
                    // boolean because "no" without saying which part is on which
                    // version sends the reader to the wrong file.
                    Ok(problems) => format!(
                        "{{\"composable\":{},\"problems\":[{}]}}",
                        problems.is_empty(),
                        problems
                            .iter()
                            .map(|p| format!("\"{}\"", esc(p)))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    Err(e) => err(e),
                }
            }

            _ => "{\"service\":\"contract-probe\",\"routes\":[\"/publish\",\"/current\",\"/get\",\"/proposed\",\"/ask\",\"/pending\",\"/answer\",\"/ratify\",\"/built-against\",\"/composable\"]}"
                .to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = stream.blocking_write_and_flush(body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
