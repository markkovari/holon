//! `secret-probe` — an instrument for `comp:secrets/reader` (see wit/probe.wit).
//!
//!   GET /has?k=stripe     was this component granted that key? (no value read)
//!   GET /reveal?k=stripe  read it — the audited call, and the only path to a value
//!
//! `/has` returning `granted:false` for a key another tenant holds is the boundary
//! ADR-0051 is about: the guest names a key, never a reference, so there is no string
//! it can send that reaches a secret nobody granted it.

#[allow(warnings)]
mod bindings;

use bindings::comp::secrets::reader;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
/// This bit a real run — a 4573-byte contract — and cost four failed starts to
/// find, so it is written the same way everywhere now.
///
/// Not a flat 4096-byte loop: `check-write` is the stream saying how much it will
/// take right now, usually far more, so this writes in whatever bites it offers,
/// waits on the pollable when it offers none, and flushes ONCE at the end.
///
/// Returns false when the stream is gone. For an SSE loop that means the client
/// hung up, which is ordinary and not an error.
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

struct Component;

/// One query parameter. A two-key query does not need a URL crate.
fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

/// JSON string escaping for the two characters a secret value could plausibly carry
/// into one. Not a general encoder — this is a probe, and a dependency for two
/// `replace` calls is the thing this repo keeps deleting.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let k = param(&query, "k");

        let body = match (request.method(), route.as_str()) {
            (Method::Get, "/has") => match reader::get(&k) {
                // `key()` is read back off the handle rather than echoed from the
                // query: it proves the handle carries the manifest's name, which is
                // the only thing about a secret that is safe to log.
                Ok(Some(s)) => {
                    format!("{{\"key\":\"{}\",\"granted\":true,\"name\":\"{}\"}}", esc(&k), esc(&s.key()))
                }
                // Not an error. An optional secret being absent is a normal way to
                // run, and it is also what a guest gets for a key it was not granted.
                Ok(None) => format!("{{\"key\":\"{}\",\"granted\":false}}", esc(&k)),
                Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
            },
            (Method::Get, "/reveal") => match reader::get(&k) {
                Ok(Some(s)) => match reader::reveal(&s) {
                    Ok(v) => format!("{{\"key\":\"{}\",\"value\":\"{}\"}}", esc(&k), esc(&v)),
                    Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
                },
                Ok(None) => format!("{{\"key\":\"{}\",\"granted\":false}}", esc(&k)),
                Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
            },
            _ => "{\"service\":\"secret-probe\",\"routes\":[\"/has?k=\",\"/reveal?k=\"]}".to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
