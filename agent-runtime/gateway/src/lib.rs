//! `agent-gateway` — HTTP in, the agent runtime out.
//!
//! Config (`wasi:config/store`, set per deployment):
//!   runtime-url   where the agent runtime listens, e.g. `http://127.0.0.1:8017`
//!   agent         which agent this deployment fronts
//!
//! `GET /ping` is forwarded as a liveness check that costs no model call.
//!
//! The caller chooses only the task text (`?q=` or a POST body). It does NOT
//! choose the path: the agent name comes from config, so a request to one
//! agent's hostname can never reach another agent or the runtime's admin API.
#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::{Guest, IncomingRequest};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, OutgoingResponse, RequestOptions,
    ResponseOutparam, Scheme,
};
use bindings::wasi::io::streams::{InputStream, StreamError};

guestio::guest_write_all!();

struct Component;

/// An agent run can take a while (model + tools); a caller would rather wait
/// than get an error while the agent is mid-thought.
const TIMEOUT_NS: u64 = 120_000_000_000;
const MAX_BODY: usize = 1 << 20;

fn read_all(stream: &InputStream, cap: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            Err(StreamError::Closed) | Err(_) => break,
        }
        if buf.len() >= cap {
            buf.truncate(cap);
            break;
        }
    }
    buf
}

fn parse_url(url: &str) -> Option<(Scheme, String, String)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else {
        (Scheme::Http, url.strip_prefix("http://")?)
    };
    match rest.find('/') {
        Some(i) => {
            Some((scheme, rest[..i].to_string(), rest[i..].trim_end_matches('/').to_string()))
        }
        None => Some((scheme, rest.to_string(), String::new())),
    }
}

/// The raw (still percent-encoded) `q=` pair of a query string, if any.
fn raw_q(path_with_query: &str) -> Option<&str> {
    let query = path_with_query.split_once('?')?.1;
    query.split('&').find(|p| p.starts_with("q="))
}

/// `(status, body)` from the runtime, or an error for the caller.
fn forward(request: &IncomingRequest) -> Result<(u16, Vec<u8>), (u16, String)> {
    let cfg = |k: &str| config::get(k).ok().flatten().filter(|v| !v.is_empty());
    let runtime =
        cfg("runtime-url").ok_or((503, "gateway has no runtime-url configured".to_string()))?;
    let agent = cfg("agent").ok_or((503, "gateway has no agent configured".to_string()))?;
    let (scheme, authority, base) =
        parse_url(&runtime).ok_or((503, "bad runtime-url".to_string()))?;

    let incoming_path = request.path_with_query().unwrap_or_default();
    let mut body = Vec::new();
    let incoming = request.consume().ok();
    if let Some(s) = incoming.as_ref().and_then(|b| b.stream().ok()) {
        body = read_all(&s, MAX_BODY);
    }
    drop(incoming);
    let route = if incoming_path.split('?').next() == Some("/ping") { "ping" } else { "run" };
    let mut path = format!("{base}/agents/{agent}/{route}");
    if let Some(q) = raw_q(&incoming_path).filter(|_| route == "run") {
        path.push('?');
        path.push_str(q);
    }

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/plain".to_vec()]);
    let req = OutgoingRequest::new(headers);
    let method = if body.is_empty() { Method::Get } else { Method::Post };
    req.set_method(&method).map_err(|_| (502, "method".to_string()))?;
    req.set_scheme(Some(&scheme)).map_err(|_| (502, "scheme".to_string()))?;
    req.set_authority(Some(&authority)).map_err(|_| (502, "authority".to_string()))?;
    req.set_path_with_query(Some(&path)).map_err(|_| (502, "path".to_string()))?;
    let out = req.body().map_err(|_| (502, "body".to_string()))?;
    if !body.is_empty() {
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, &body);
        }
    }
    let _ = OutgoingBody::finish(out, None);

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));
    let fut = outgoing_handler::handle(req, Some(opts))
        .map_err(|e| (502, format!("could not reach the agent runtime: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut.get().and_then(|r| r.ok()).and_then(|r| r.ok()).ok_or((
        502,
        "the agent runtime did not answer (is it up, and is it in this tenant's egress list?)"
            .to_string(),
    ))?;
    let status = resp.status();
    // The body must outlive its stream: dropping a parent resource while a
    // child still exists traps with "resource has children". `body` is
    // declared first, so it is dropped last.
    let body = resp.consume().ok();
    let bytes = match body.as_ref().and_then(|b| b.stream().ok()) {
        Some(s) => {
            let v = read_all(&s, MAX_BODY);
            drop(s);
            v
        }
        None => Vec::new(),
    };
    Ok((status, bytes))
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let (status, body) = match forward(&request) {
            Ok(r) => r,
            Err((code, msg)) => (code, msg.into_bytes()),
        };
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"text/plain; charset=utf-8".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, &body);
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
