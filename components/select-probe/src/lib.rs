//! `select-probe` — an instrument for `graph:select` (see wit/probe.wit).
//!
//!   POST /select  {entries:[…]}                 — decide, without acting
//!   POST /land    {entries:[…], landing:{…}}    — decide, and propose the winner
//!
//! Both, because the assertion that matters most is a NEGATIVE one: when nothing
//! passed the gate, the forge must see no request at all. That is only checkable
//! against something that would have made one.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../git-forge/wit",
            "../llm-inference/wit",
            "../graph-agent/wit",
            "../graph-select/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "comp:selectprobe/select-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::graph::select::selector as sel;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Request, Response};
use serde_json::json;

struct Component;

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

fn entries_of(v: &serde_json::Value) -> Vec<sel::Entry> {
    v["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| sel::Entry {
            branch: e["branch"].as_str().unwrap_or_default().to_string(),
            accepted: e["accepted"].as_bool().unwrap_or(false),
            score: e["score"].as_u64().unwrap_or(0) as u32,
            digest: e["digest"].as_str().unwrap_or_default().to_string(),
            spent_tokens: e["spent_tokens"].as_u64().unwrap_or(0) as u32,
            attempts: e["attempts"].as_u64().unwrap_or(0) as u32,
            files: e["files"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|f| sel::File {
                    path: f["path"].as_str().unwrap_or_default().to_string(),
                    content: f["content"].as_str().unwrap_or_default().to_string(),
                })
                .collect(),
        })
        .collect()
}

fn outcome_json(o: &sel::Outcome) -> serde_json::Value {
    let mut out = json!({
        "distinct": o.distinct,
        "accepted": o.accepted,
        "spent_tokens": o.spent_tokens,
    });
    match &o.decision {
        sel::Decision::Winner(c) => {
            out["winner"] = json!({ "index": c.index, "branch": c.branch, "because": c.because });
        }
        sel::Decision::NothingAcceptable(why) => {
            out["nothing_acceptable"] = json!(why);
        }
    }
    out
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = match route.as_str() {
            "/select" => {
                let v: serde_json::Value = serde_json::from_str(&read_body(request).await)
                    .unwrap_or(serde_json::Value::Null);
                match sel::select(&entries_of(&v)) {
                    Ok(o) => outcome_json(&o).to_string(),
                    Err(sel::SelectError::Invalid(m)) => {
                        json!({ "error": "invalid", "detail": m }).to_string()
                    }
                }
            }
            "/land" => {
                let v: serde_json::Value = serde_json::from_str(&read_body(request).await)
                    .unwrap_or(serde_json::Value::Null);
                let l = &v["landing"];
                let landing = sel::Landing {
                    branch: l["branch"].as_str().unwrap_or("candidate").to_string(),
                    base: l["base"].as_str().unwrap_or_default().to_string(),
                    title: l["title"].as_str().unwrap_or("a candidate").to_string(),
                    body: l["body"].as_str().unwrap_or_default().to_string(),
                    message: l["message"].as_str().unwrap_or("a candidate").to_string(),
                };
                match sel::land(&entries_of(&v), &landing) {
                    Ok(o) => json!({
                        "number": o.number, "url": o.url, "commit": o.commit, "branch": o.branch,
                    })
                    .to_string(),
                    Err(e) => {
                        let (kind, detail) = match e {
                            sel::LandError::NothingAcceptable(m) => ("nothing-acceptable", m),
                            sel::LandError::Forge(m) => ("forge", m),
                            sel::LandError::Invalid(m) => ("invalid", m),
                        };
                        json!({ "error": kind, "detail": detail }).to_string()
                    }
                }
            }
            _ => json!({ "service": "select-probe", "routes": ["/select", "/land"] }).to_string(),
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
