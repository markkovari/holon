//! `agent-probe` — an instrument for `graph:agent` (see wit/probe.wit).
//!
//!   POST /attempt   {text, writable[], context[], previous[], seed}
//!
//! The interesting call is the second one: the same goal with a `previous`
//! failure. If the answer is identical, the repair loop is a re-roll.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../llm-inference/wit",
            "../graph-agent/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "comp:agentprobe/agent-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::graph::agent::writer as agent;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Request, Response};
use serde_json::json;

guestio::guest_p3_respond!();

struct Component;

/// Probe input, not a user upload: 16 MiB is far past any goal it is sent.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = if route == "/attempt" {
            let raw = read_body(request).await;
            let v: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let files = |key: &str| -> Vec<agent::File> {
                v[key]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|f| agent::File {
                        path: f["path"].as_str().unwrap_or_default().to_string(),
                        content: f["content"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            };
            let g = agent::Goal {
                text: v["text"].as_str().unwrap_or_default().to_string(),
                context: files("context"),
                writable: v["writable"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|s| s.as_str().unwrap_or_default().to_string())
                    .collect(),
            };
            let previous: Vec<agent::Failure> = v["previous"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|f| agent::Failure {
                    id: f["id"].as_str().unwrap_or_default().to_string(),
                    detail: f["detail"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            // Optional in the request, so every existing caller of this probe keeps
            // working — a probe is a door onto an interface, and a door that broke
            // every knock because the interface grew a parameter would be its own
            // kind of debt.
            let blocked: Vec<agent::Blocked> = v["blocked"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|b| agent::Blocked {
                    id: b["id"].as_str().unwrap_or_default().to_string(),
                    needs: b["needs"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            let seed = v["seed"].as_u64().unwrap_or(0);

            match agent::attempt(&g, &previous, &blocked, seed) {
                Ok(c) => json!({
                    "files": c.files.iter().map(|f| json!({ "path": f.path, "content": f.content }))
                        .collect::<Vec<_>>(),
                    "prompt_tokens": c.prompt_tokens,
                    "completion_tokens": c.completion_tokens,
                    "model": c.model,
                })
                .to_string(),
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

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
