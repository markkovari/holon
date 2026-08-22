//! eshop:gateway — the Envoy + Blazor-host stand-in: embedded storefront SPA
//! plus prefix-routed forwarding via the proxy:route capability (which owns
//! the route table and the outgoing-HTTP round trip; this component is glue).
//!
//! POST /internal/pump fans out to every consumer service's pump (the
//! /pump/* routes in the table), so one driver advances the whole
//! choreography.

#[allow(warnings)]
mod bindings;

use bindings::proxy::route::router;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

// ponytail: single-file SPA (the jco-helpdesk pattern) include_str!'d here;
// switch to the static-assets component if the UI ever needs a build step.
const INDEX_HTML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/eshop/ui/index.html"));

/// Ordering first (creates/advances), then the reactors.
const PUMPS: [&str; 4] = ["/pump/ordering", "/pump/catalog", "/pump/payment", "/pump/basket"];

struct Component;

impl bindings::exports::wasi::http::incoming_handler::Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = method_str(&request.method());
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match seg.as_slice() {
            ["api", ..] => {
                let mut headers = Vec::new();
                if let Some(auth) = header(&request, "authorization") {
                    headers.push(("authorization".to_string(), auth));
                }
                let body = match method.as_str() {
                    "GET" | "HEAD" => Vec::new(),
                    _ => read_body(&request),
                };
                match router::forward(&method, &path, &headers, &body) {
                    Ok(up) => respond(response_out, up.status, &up.content_type, &up.body),
                    Err(router::ProxyError::NoRoute) => {
                        respond(response_out, 404, "application/json", b"{\"error\":\"not_found\"}")
                    }
                    Err(router::ProxyError::UpstreamUnreachable(m)) => respond(
                        response_out,
                        502,
                        "application/json",
                        format!("{{\"error\":\"upstream unreachable: {m}\"}}").as_bytes(),
                    ),
                }
            }
            ["internal", "pump"] => {
                let ok = PUMPS
                    .iter()
                    .filter(|p| router::forward("POST", p, &[], &[]).is_ok())
                    .count();
                let body = format!("{{\"pumped\":{ok}}}");
                respond(response_out, 200, "application/json", body.as_bytes());
            }
            // everything else is the storefront (SPA fallback included).
            _ => respond(response_out, 200, "text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        }
    }
}

fn method_str(m: &Method) -> String {
    match m {
        Method::Get => "GET".into(),
        Method::Post => "POST".into(),
        Method::Put => "PUT".into(),
        Method::Delete => "DELETE".into(),
        Method::Patch => "PATCH".into(),
        Method::Head => "HEAD".into(),
        Method::Options => "OPTIONS".into(),
        Method::Trace => "TRACE".into(),
        Method::Connect => "CONNECT".into(),
        Method::Other(s) => s.clone(),
    }
}

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request
        .headers()
        .get(&name.to_string())
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
}

/// A ceiling on a request body, not a policy: past this the read stops and the
/// caller is told, rather than growing until the store's memory cap traps the
/// component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(body) = request.consume() {
        if let Ok(stream) = body.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        if buf.len() + c.len() > MAX_BODY_BYTES {
                            // No error channel on this one, so an over-long body reads as
                            // EMPTY rather than as a plausible prefix of itself.
                            return Vec::new();
                        }
                        buf.extend_from_slice(&c);
                    }
                    Err(bindings::wasi::io::streams::StreamError::Closed) => break,
                    // A failed read is not an end of body: collapsing the two
                    // returns a truncated payload as if it were whole.
                    // A failed read is NOT the end of a body. Breaking here returns
                    // what arrived so far as though it were complete.
                    Err(_) => return Vec::new(),
                }
            }
        }
    }
    buf
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
