//! `fitness-probe` — an instrument for `graph:fitness` (see wit/probe.wit).
//!
//!   POST /evaluate   body is {name, base_commit, base_tree[], changes[], checks[]}
//!
//! Errors answer 200 with `{"error":...}`, because which of the three they are is
//! the interesting output: `need-base` means send the tree, `invalid` means fix
//! the request, `unavailable` means the runner is not there — three different
//! next moves that a status code would flatten into one.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../graph-fitness/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "comp:fitnessprobe/fitness-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::graph::fitness::evaluator as fitness;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Request, Response};
use serde_json::json;

struct Component;

fn files(v: &serde_json::Value, key: &str) -> Vec<fitness::File> {
    v[key]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| fitness::File {
            path: f["path"].as_str().unwrap_or_default().to_string(),
            content: f["content"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = if route == "/evaluate" {
            let raw = read_body(request).await;
            let v: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let candidate = fitness::Candidate {
                name: v["name"].as_str().unwrap_or_default().to_string(),
                base_commit: v["base_commit"].as_str().unwrap_or_default().to_string(),
                base_tree: files(&v, "base_tree"),
                changes: files(&v, "changes"),
            };
            let checks: Vec<fitness::Check> = v["checks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|c| fitness::Check {
                    id: c["id"].as_str().unwrap_or_default().to_string(),
                    required: c["required"].as_bool().unwrap_or(false),
                    weight: c["weight"].as_u64().unwrap_or(1) as u32,
                    command: c["command"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|a| a.as_str().unwrap_or_default().to_string())
                        .collect(),
                    // The edges, so the graph can be driven from outside this
                    // repository's own callers.
                    needs: c["needs"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|a| a.as_str().unwrap_or_default().to_string())
                        .collect(),
                })
                .collect();

            match fitness::evaluate(&candidate, &checks) {
                Ok(v) => json!({
                    "accepted": v.accepted,
                    "score": v.score,
                    "outcomes": v.outcomes.iter().map(|o| json!({
                        "id": o.id, "required": o.required, "weight": o.weight,
                        "state": match o.state {
                            fitness::CheckState::Passed => "passed",
                            fitness::CheckState::Failed => "failed",
                            fitness::CheckState::NotAttempted => "not-attempted",
                        },
                        "blocked_by": o.blocked_by,
                        "detail": o.detail,
                    })).collect::<Vec<_>>(),
                })
                .to_string(),
                Err(e) => {
                    let (kind, detail) = match e {
                        fitness::EvalError::Unavailable(m) => ("unavailable", m),
                        fitness::EvalError::Invalid(m) => ("invalid", m),
                        fitness::EvalError::NeedBase(m) => ("need-base", m),
                    };
                    json!({ "error": kind, "detail": detail }).to_string()
                }
            }
        } else {
            json!({ "service": "fitness-probe", "routes": ["/evaluate"] }).to_string()
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
