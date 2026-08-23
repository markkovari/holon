//! `docker-manager-domain` — list running containers and their state over HTTP

#[allow(warnings)]
mod bindings;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::os::container::docker;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::keyvalue::store;
use serde_json::json;

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Html(200, r#"<!DOCTYPE html><html><head><title>Docker Manager</title></head><body><h1>Docker Manager</h1><button onclick="fetch('/api/ps').then(r=>r.json()).then(d=>document.getElementById('r').innerText=JSON.stringify(d))">List Containers</button><div id="r"></div></body></html>"#.to_string()),
            (Method::Get, ["api", "ps"]) => {
                
        let outcome = docker::ps();
        Outcome::Json(200, json!({ "containers": outcome }).to_string())
        
            },
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b, b"text/html".to_vec()),
        Outcome::Json(c, b) => (c, b, b"application/json".to_vec()),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string(), b"application/json".to_vec()),
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}
bindings::export!(Component with_types_in bindings);

/// Write every byte, respecting what the stream says it can take.
///
/// `blocking_write_and_flush` accepts at most 4096 bytes and TRAPS above it,
/// which kills the component mid-response — the caller sees a closed connection
/// and no status. Any page or JSON body larger than 4 KiB hits it, so the size
/// of the payload decides whether the endpoint works.
///
/// `check_write` reports what the stream will accept now; a zero means block on
/// the pollable and ask again. Copied from the shape every other domain here
/// already uses.
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
