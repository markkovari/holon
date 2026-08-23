//! `proxy-route` — forward a request to an upstream by path prefix — a config-driven reverse proxy
//! (`routes`, `prefix=upstream` comma-separated, longest prefix wins;
//! upstream ending in `/` strips the matched prefix), forwarding over the
//! host's wasi:http outgoing handler.

#[allow(warnings)]
mod bindings;

use bindings::exports::proxy::route::router::{Guest, ProxyError, UpstreamResponse};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

fn table() -> Vec<(String, String)> {
    let raw = config::get("routes").ok().flatten().unwrap_or_default();
    let mut entries: Vec<(String, String)> = raw
        .split(',')
        .filter_map(|e| {
            let (prefix, upstream) = e.trim().split_once('=')?;
            (!prefix.is_empty() && !upstream.is_empty())
                .then(|| (prefix.to_string(), upstream.to_string()))
        })
        .collect();
    // longest prefix first, so the first match wins.
    entries.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    entries
}

/// Apply the strip convention: upstream ending in `/` drops the matched
/// prefix, otherwise the full original path rides along.
fn target_for(path_with_query: &str, prefix: &str, upstream: &str) -> String {
    if let Some(base) = upstream.strip_suffix('/') {
        let rest = &path_with_query[prefix.len()..];
        if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') {
            format!("{base}{rest}")
        } else {
            format!("{base}/{rest}")
        }
    } else {
        format!("{upstream}{path_with_query}")
    }
}

fn resolve_route(path_with_query: &str) -> Option<String> {
    let path = path_with_query.split('?').next().unwrap_or(path_with_query);
    table()
        .into_iter()
        .find(|(prefix, _)| {
            path == prefix || path.starts_with(&format!("{prefix}/")) || prefix == "/"
        })
        .map(|(prefix, upstream)| target_for(path_with_query, &prefix, &upstream))
}

impl Guest for Component {
    fn resolve(path: String) -> Option<String> {
        resolve_route(&path)
    }

    fn forward(
        method: String,
        path_with_query: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<UpstreamResponse, ProxyError> {
        let target = resolve_route(&path_with_query).ok_or(ProxyError::NoRoute)?;
        fetch(&method, &target, &headers, &body)
    }
}

fn net(ctx: &str) -> ProxyError {
    ProxyError::UpstreamUnreachable(ctx.to_string())
}

fn fetch(
    method: &str,
    url: &str,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> Result<UpstreamResponse, ProxyError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(net(&format!("bad url scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    let m = match method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "PATCH" => Method::Patch,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        other => Method::Other(other.to_string()),
    };

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra_headers {
        let _ = headers.set(k, &[v.as_bytes().to_vec()]);
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&m).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&path)).map_err(|_| net("set path"))?;
    {
        let out = req.body().map_err(|_| net("body"))?;
        if !body.is_empty() {
            let stream = out.write().map_err(|_| net("write stream"))?;
            for chunk in body.chunks(4096) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|e| net(&format!("body write: {e:?}")))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
    }

    let future = outgoing_handler::handle(req, Some(RequestOptions::new()))
        .map_err(|e| net(&format!("handle: {e:?}")))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_else(|| "application/json".into());
    let mut bytes = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => bytes.extend_from_slice(&c),
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok(UpstreamResponse { status, content_type, body: bytes })
}

bindings::export!(Component with_types_in bindings);
