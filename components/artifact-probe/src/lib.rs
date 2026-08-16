//! `artifact-probe` — an instrument for `artifact:cache` (see wit/probe.wit).
//!
//!   GET  /lookup?producer=&version=&inputs=&params=   hit | claimed | pending
//!   GET  /id?…same…                                   the derived id, no store touched
//!   POST /put?claim=            body is the artifact
//!   GET  /get?id=
//!   POST /abandon?claim=

#[allow(warnings)]
mod bindings;

use bindings::artifact::cache::store as cache;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

/// `inputs` is comma-separated: enough for a probe, and it keeps the ordering
/// visible in the URL, which is a property the cache cares about.
fn key_from(query: &str) -> cache::ArtifactKey {
    let raw = param(query, "inputs");
    cache::ArtifactKey {
        producer: param(query, "producer"),
        version: param(query, "version"),
        inputs: if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(',').map(str::to_string).collect()
        },
        params: param(query, "params"),
    }
}

fn err(e: cache::CacheError) -> String {
    let (kind, msg) = match e {
        cache::CacheError::Unavailable(m) => ("unavailable", m),
        cache::CacheError::Invalid(m) => ("invalid", m),
        cache::CacheError::NotYourClaim(m) => ("not-your-claim", m),
    };
    json!({ "error": kind, "detail": msg }).to_string()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: IncomingRequest) -> Vec<u8> {
    let Ok(body) = request.consume() else { return Vec::new() };
    let Ok(stream) = body.stream() else { return Vec::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // Same reasoning as the error arm below: an over-long body reads
                // as empty rather than as a plausible prefix of itself.
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return Vec::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller reading an empty body fails cleanly, where half an
            // artifact is a plausible-looking file that is not the one uploaded.
            Err(_) => return Vec::new(),
        }
    }
    out
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
            (Method::Get, "/id") => json!({ "id": cache::derive_id(&key_from(&query)) }).to_string(),
            (Method::Get, "/lookup") => match cache::lookup(&key_from(&query)) {
                Ok(cache::Outcome::Hit(a)) => json!({
                    "state": "hit",
                    "id": a.id,
                    "content": String::from_utf8_lossy(&a.bytes),
                    "producer": a.producer,
                })
                .to_string(),
                Ok(cache::Outcome::Claimed(token)) => {
                    json!({ "state": "claimed", "claim": token }).to_string()
                }
                Ok(cache::Outcome::Pending(ms)) => {
                    json!({ "state": "pending", "retry_ms": ms }).to_string()
                }
                Err(e) => err(e),
            },
            (Method::Post, "/put") => {
                let claim = param(&query, "claim");
                let bytes = read_body(request);
                match cache::put(&claim, &bytes, "text/plain") {
                    Ok(id) => json!({ "stored": id }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Post, "/abandon") => match cache::abandon(&param(&query, "claim")) {
                Ok(()) => json!({ "abandoned": true }).to_string(),
                Err(e) => err(e),
            },
            (Method::Get, "/get") => match cache::get(&param(&query, "id")) {
                Ok(Some(a)) => json!({
                    "id": a.id, "content": String::from_utf8_lossy(&a.bytes),
                })
                .to_string(),
                Ok(None) => json!({ "found": false }).to_string(),
                Err(e) => err(e),
            },
            _ => json!({
                "service": "artifact-probe",
                "routes": ["/lookup", "/id", "/put", "/get", "/abandon"]
            })
            .to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            for chunk in body.as_bytes().chunks(4096) {
                let _ = stream.blocking_write_and_flush(chunk);
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
