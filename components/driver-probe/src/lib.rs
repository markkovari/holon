//! `driver-probe` — an instrument for `graph:run` (see wit/probe.wit).
//!
//!   POST /run  {text, writable[], context[], checks[], base_commit,
//!               base_tree[], max_attempts, seed}
//!
//! The whole attempt log comes back, because the interesting assertions are
//! about the SHAPE of a run — how many attempts it took, whether attempt two
//! differed from attempt one, why it stopped — and none of that is visible in the
//! candidate it returns.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::graph::run::driver as run;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

struct Component;

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // Same reasoning as the error arm below: an over-long body reads
                // as empty rather than as a plausible prefix of itself.
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a
            // JSON document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn files(v: &serde_json::Value, key: &str) -> Vec<run::File> {
    v[key]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| run::File {
            path: f["path"].as_str().unwrap_or_default().to_string(),
            content: f["content"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| s.as_str().unwrap_or_default().to_string())
        .collect()
}

fn plan_of(v: &serde_json::Value) -> run::Plan {
    run::Plan {
        goal: run::Goal {
            text: v["text"].as_str().unwrap_or_default().to_string(),
            context: files(v, "context"),
            writable: strings(v, "writable"),
        },
        previous: v["previous"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|f| run::Failure {
                id: f["id"].as_str().unwrap_or_default().to_string(),
                detail: f["detail"].as_str().unwrap_or_default().to_string(),
            })
            .collect(),
        checks: v["checks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| run::Check {
                id: c["id"].as_str().unwrap_or_default().to_string(),
                required: c["required"].as_bool().unwrap_or(true),
                weight: c["weight"].as_u64().unwrap_or(1) as u32,
                command: strings(c, "command"),
            })
            .collect(),
        base_commit: v["base_commit"].as_str().unwrap_or_default().to_string(),
        base_tree: files(v, "base_tree"),
        max_attempts: v["max_attempts"].as_u64().unwrap_or(1) as u32,
        max_tokens: v["max_tokens"].as_u64().unwrap_or(0) as u32,
        patience: v["patience"].as_u64().unwrap_or(0) as u32,
        seed: v["seed"].as_u64().unwrap_or(0),
    }
}

fn report(r: &run::RunResult) -> serde_json::Value {
    json!({
        "accepted": r.accepted,
        "score": r.score,
        "stopped": match r.stopped {
            run::StopReason::Accepted => "accepted",
            run::StopReason::Exhausted => "exhausted",
            run::StopReason::Plateau => "plateau",
            run::StopReason::NoProgress => "no-progress",
            run::StopReason::OverBudget => "over-budget",
        },
        "spent_tokens": r.spent_tokens,
        "files": r.files.iter()
            .map(|f| json!({ "path": f.path, "content": f.content })).collect::<Vec<_>>(),
        "failures": r.failures.iter()
            .map(|f| json!({ "id": f.id, "detail": f.detail })).collect::<Vec<_>>(),
        "attempts": r.attempts.iter().map(|a| json!({
            "seed": a.seed,
            "digest": a.digest,
            "score": a.score,
            "accepted": a.accepted,
            "error": a.error,
            "prompt_tokens": a.prompt_tokens,
            "completion_tokens": a.completion_tokens,
            "model": a.model,
        })).collect::<Vec<_>>(),
    })
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = if route == "/run" {
            let raw = read_body(request);
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            match run::run(&plan_of(&v)) {
                Ok(r) => report(&r).to_string(),
                Err(e) => {
                    let (kind, detail) = match e {
                        run::RunError::ProviderDown(m) => ("provider-down", m),
                        run::RunError::GateUnusable(m) => ("gate-unusable", m),
                        run::RunError::Invalid(m) => ("invalid", m),
                    };
                    json!({ "error": kind, "detail": detail }).to_string()
                }
            }
        } else {
            json!({ "service": "driver-probe", "routes": ["/run"] }).to_string()
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
