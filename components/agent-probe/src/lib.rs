//! `agent-probe` — an instrument for `graph:agent` (see wit/probe.wit).
//!
//!   POST /attempt   {text, writable[], context[], previous[], seed}
//!
//! The interesting call is the second one: the same goal with a `previous`
//! failure. If the answer is identical, the repair loop is a re-roll.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::graph::agent::writer as agent;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

struct Component;

fn read_body(request: IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    // `while let Ok(..)` treats a failed read exactly like the end of the body.
    // See platform-domain for the shape that distinguishes them; this probe reads
    // its own test input, so a truncated read shows up as a failed assertion
    // rather than as data loss.
    while let Ok(chunk) = stream.blocking_read(64 * 1024) {
        if chunk.is_empty() { break; }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = if route == "/attempt" {
            let raw = read_body(request);
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let files = |key: &str| -> Vec<agent::File> {
                v[key].as_array().cloned().unwrap_or_default().iter()
                    .map(|f| agent::File {
                        path: f["path"].as_str().unwrap_or_default().to_string(),
                        content: f["content"].as_str().unwrap_or_default().to_string(),
                    }).collect()
            };
            let g = agent::Goal {
                text: v["text"].as_str().unwrap_or_default().to_string(),
                context: files("context"),
                writable: v["writable"].as_array().cloned().unwrap_or_default().iter()
                    .map(|s| s.as_str().unwrap_or_default().to_string()).collect(),
            };
            let previous: Vec<agent::Failure> = v["previous"].as_array().cloned().unwrap_or_default()
                .iter().map(|f| agent::Failure {
                    id: f["id"].as_str().unwrap_or_default().to_string(),
                    detail: f["detail"].as_str().unwrap_or_default().to_string(),
                }).collect();
            let seed = v["seed"].as_u64().unwrap_or(0);

            match agent::attempt(&g, &previous, seed) {
                Ok(c) => json!({
                    "files": c.files.iter().map(|f| json!({ "path": f.path, "content": f.content }))
                        .collect::<Vec<_>>(),
                    "prompt_tokens": c.prompt_tokens,
                    "completion_tokens": c.completion_tokens,
                    "model": c.model,
                }).to_string(),
                Err(e) => {
                    let (kind, detail) = match e {
                        agent::AgentError::InferenceFailed(m) => ("inference-failed", m),
                        agent::AgentError::UnderSpecified(m) => ("under-specified", m),
                        agent::AgentError::UnusableAnswer(m) => ("unusable-answer", m),
                    };
                    json!({ "error": kind, "detail": detail }).to_string()
                }
            }
        } else {
            json!({ "service": "agent-probe", "routes": ["/attempt"] }).to_string()
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            for chunk in body.as_bytes().chunks(4096) { let _ = stream.blocking_write_and_flush(chunk); }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
