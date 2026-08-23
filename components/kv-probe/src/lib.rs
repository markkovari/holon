//! `kv-probe` — an instrument for one specific unknown (see wit/probe.wit).
//!
//! The bucket it opens comes from `wasi:config` (`bucket`, default `"default"`),
//! because the question is what the operator's `hostInterfaces[].name` does, and
//! that cannot be asked by a component with the name compiled in.
//!
//!   GET /who        which bucket name it opened, and whether the open SUCCEEDED
//!   GET /put?k=&v=  write
//!   GET /get?k=     read (`found: false` is a miss, not an error)
//!
//! `/who` reporting an open failure is the interesting outcome, not a bug: it means
//! a name no `hostInterfaces` entry declares fails closed rather than falling back
//! to a shared store.

#[allow(warnings)]
mod bindings;

use bindings::wasi::keyvalue::batch;
use bindings::wasi::keyvalue::store as kv;

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

/// The bucket comes from the QUERY, not from `wasi:config`.
///
/// Deliberate: the question is what `hostInterfaces[].name` does, and mixing in a
/// second host interface this host does not advertise (`wasi:config` is absent from
/// its "Host provides interfaces" list) would confound a link failure with a naming
/// failure. `?bucket=` keeps the probe's world to http + keyvalue and nothing else.
fn bucket_of(query: &str) -> String {
    let b = param(query, "bucket");
    if b.is_empty() { "default".to_string() } else { b }
}

/// One query parameter, no dependency on a URL crate for a two-key query.
fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let name = bucket_of(&query);

        let body = match (request.method(), route.as_str()) {
            (Method::Get, "/who") => match kv::open(&name) {
                Ok(_) => format!("{{\"bucket\":\"{name}\",\"open\":\"ok\"}}"),
                // The whole point of the probe.
                Err(e) => format!("{{\"bucket\":\"{name}\",\"open\":\"FAILED\",\"error\":\"{e:?}\"}}"),
            },
            (Method::Get, "/put") => {
                let (k, v) = (param(&query, "k"), param(&query, "v"));
                match kv::open(&name).and_then(|b| b.set(&k, v.as_bytes())) {
                    Ok(()) => format!("{{\"bucket\":\"{name}\",\"put\":\"{k}\",\"value\":\"{v}\"}}"),
                    Err(e) => format!("{{\"bucket\":\"{name}\",\"error\":\"{e:?}\"}}"),
                }
            }
            // Keeps the imported `bucket` resource the same shape as `record-store`'s
            // {get,set,delete}, since a component's embedded type only carries the
            // methods it references. This was a suspect for the bind failure and was
            // NOT the cause — `hostInterfaces[].name` was (ADR-0015). Whether a
            // strict subset also fails is untested; matching a known-good shape keeps
            // that variable out of future runs.
            (Method::Get, "/del") => {
                let k = param(&query, "k");
                match kv::open(&name).and_then(|b| b.delete(&k)) {
                    Ok(()) => format!("{{\"bucket\":\"{name}\",\"deleted\":\"{k}\"}}"),
                    Err(e) => format!("{{\"bucket\":\"{name}\",\"error\":\"{e:?}\"}}"),
                }
            }
            // Exists so `batch` is retained in the imported type, matching the
            // components that do bind on this host.
            (Method::Get, "/many") => {
                let k = param(&query, "k");
                match kv::open(&name).and_then(|b| batch::get_many(&b, std::slice::from_ref(&k))) {
                    Ok(v) => format!("{{\"bucket\":\"{name}\",\"many\":{}}}", v.len()),
                    Err(e) => format!("{{\"bucket\":\"{name}\",\"error\":\"{e:?}\"}}"),
                }
            }
            (Method::Get, "/get") => {
                let k = param(&query, "k");
                match kv::open(&name).and_then(|b| b.get(&k)) {
                    Ok(Some(v)) => format!(
                        "{{\"bucket\":\"{name}\",\"key\":\"{k}\",\"found\":true,\"value\":\"{}\"}}",
                        String::from_utf8_lossy(&v)
                    ),
                    Ok(None) => {
                        format!("{{\"bucket\":\"{name}\",\"key\":\"{k}\",\"found\":false}}")
                    }
                    Err(e) => format!("{{\"bucket\":\"{name}\",\"error\":\"{e:?}\"}}"),
                }
            }
            _ => format!(
                "{{\"service\":\"kv-probe\",\"bucket\":\"{name}\",\"routes\":[\"/who\",\"/put?k=&v=\",\"/get?k=\"]}}"
            ),
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
