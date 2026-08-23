//! `golem-bridge` — durable:workflow/orchestrator over `wasi:http`, to Golem.
//!
//! Satisfies the same execution seam as the in-process backend and the native
//! provider, but runs as a component on the wasmCloud v2 operator: `trigger`
//! POSTs the job payload to a durable Golem worker's gateway endpoint (the same
//! HTTP call `providers/golem-workflow` makes) and returns its result. The v2
//! host supplies `wasi:http/outgoing-handler`; Golem's address comes from
//! `wasi:config`. `start`/`status` (async) are left to the native provider.

#[allow(warnings)]
mod bindings;

use bindings::exports::durable::workflow::orchestrator::{Guest, RunError, RunRequest, RunStatus};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

fn cfg(key: &str, default: &str) -> String {
    match config::get(key) {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

/// Split a base URL into (scheme, authority) — e.g. `http://127.0.0.1:9006`.
fn parse_base(url: &str) -> (Scheme, String) {
    if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest.trim_end_matches('/').to_string())
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest.trim_end_matches('/').to_string())
    } else {
        (Scheme::Http, url.trim_end_matches('/').to_string())
    }
}

/// POST `body` to the Golem gateway; return (status, response bytes).
fn golem_post(path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), RunError> {
    let base = cfg("golem-url", "http://127.0.0.1:9006");
    let (scheme, authority) = parse_base(&base);
    let net = |m: &str| RunError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    // Golem gateway subdomain routing: CONNECT to the golem-url authority, but
    // send the agent's vhost as the Host header (e.g. golem-agent.localhost:9006).
    // These are distinct — conflating them dials a name the pod can't resolve.
    let host = cfg("golem-host", "");
    if !host.is_empty() {
        let _ = headers.set("host", &[host.into_bytes()]);
    }

    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(path)).map_err(|_| net("set path"))?;

    {
        let out = req.body().map_err(|_| net("body"))?;
        {
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
        .map_err(|e| RunError::Unavailable(format!("golem unreachable: {e:?}")))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| RunError::Unavailable(format!("http: {e:?}")))?;

    let status = resp.status();
    let mut buf = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => buf.extend_from_slice(&c),
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok((status, buf))
}

impl Guest for Component {
    fn trigger(req: RunRequest) -> Result<String, RunError> {
        let tmpl = cfg("golem-path-template", "/counters/{workflow-id}/increment");
        let path = tmpl.replace("{workflow-id}", &req.workflow_id);
        let (status, body) = golem_post(&path, req.payload.as_bytes())?;
        let snippet = || String::from_utf8_lossy(&body).chars().take(300).collect::<String>();
        match status {
            200..=299 => Ok(String::from_utf8_lossy(&body).into_owned()),
            404 => Err(RunError::NotFound(format!("golem 404: {}", snippet()))),
            400 | 422 => Err(RunError::InvalidInput(format!("golem {status}: {}", snippet()))),
            _ => Err(RunError::WorkerFailed(format!("golem {status}: {}", snippet()))),
        }
    }

    fn start(_req: RunRequest) -> Result<String, RunError> {
        Err(RunError::Unavailable(
            "golem-bridge is synchronous over http; async start/status is the native provider"
                .into(),
        ))
    }

    fn status(_run_id: String) -> Result<RunStatus, RunError> {
        Err(RunError::Unavailable("golem-bridge has no async runs".into()))
    }
}

bindings::export!(Component with_types_in bindings);
