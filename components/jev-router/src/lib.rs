//! `jev-router` — deterministic intent-routing gateway over `jev:decision`.
//!
//! Where `intent-router` classifies by prompting a chat model (generative,
//! `llm:inference`), this component classifies with a discrete decision call
//! (`jev:decision/decision`) — no prompt, no free-form text, just a closed set
//! of candidate domains and a typed confidence. The two stay separate
//! components on purpose: this is the "System One" fast path, and nothing
//! generative ever runs in it.
//!
//! ## The routing contract
//!
//! `POST /api/route {"query": "..."}` always returns 200 with one of:
//!   - `{"domain": <domain>, "decision": "route", "confidence": <0.0-1.0>}`
//!     when the provider is confident and its distribution isn't flat.
//!   - `{"domain": "unknown", "decision": "disambiguate", "confidence": <c>,
//!      "distribution": [...]}` otherwise.
//!
//! Dispatching the request to whichever domain was selected — `helpdesk-domain`,
//! `agent-writer`, or anything else — is deliberately NOT this component's job.
//! The Component Model requires static imports, and the plausible dispatch
//! targets export mutually incompatible interfaces (`graph:agent/writer` vs.
//! `wasi:http/incoming-handler`), so there is no single generic call this
//! component could make to "the" selected domain. `intent-router` already
//! draws this same boundary — a router's contract ends at producing the
//! decision; a caller or gateway/composition layer does the forwarding.
//!
//! Config (wasi:config/store):
//!   jev:confidence-threshold  milli-units, 0-1000 (default 650). Below this,
//!                             or when the provider itself reports a flat
//!                             distribution, the caller gets "disambiguate"
//!                             instead of a route.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../jev-decision/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "holon:jev-router/jev-router",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::jev::decision::decision::{self as jev, ChoiceRequest, DecisionError};
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::wasi::config::store as config;

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").trim_matches('/');
        let result = match (&request.get_method(), route) {
            (Method::Post, "api/route") => route_intent(request).await,
            (Method::Get, "health") => Outcome::Json(200, json!({"status": "ok"}).to_string()),
            _ => Outcome::NotFound,
        };
        emit(result)
    }
}

enum Outcome {
    Json(u16, String),
    Bad(String),
    Err(u16, String),
    NotFound,
}

#[derive(Deserialize)]
struct RouteReq {
    query: String,
}

/// The only domains this router will ever offer a decision provider. Unlike
/// `intent-router`'s `KNOWN_DOMAINS`, `"unknown"` is not among them — it is
/// never something a provider *selects*, only what this router reports when
/// it isn't confident enough in whatever the provider did select.
const ROUTABLE_DOMAINS: &[&str] =
    &["helpdesk-domain", "clinic-domain", "studio-domain", "grocery-domain"];

/// The Choice question's `instructions` — what the provider is deciding,
/// distinct from `state` (the query text itself).
const ROUTING_INSTRUCTIONS: &str = "Which backend domain should handle this query?";

/// Milli-units, 0..=1000. Below this — or a provider-reported flat
/// distribution — the caller gets "disambiguate" rather than a route.
const DEFAULT_THRESHOLD: u32 = 650;

fn confidence_threshold() -> u32 {
    config::get("jev:confidence-threshold")
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n <= 1000)
        .unwrap_or(DEFAULT_THRESHOLD)
}

/// 1 to 1000 characters — long enough for a real question, short enough that
/// a caller cannot turn this into a place to paste a document.
fn validate_query(query: &str) -> Result<(), String> {
    if query.is_empty() || query.len() > 1000 {
        return Err("query must be between 1 and 1000 characters".into());
    }
    Ok(())
}

fn distribution_json(dist: &[jev::OptionScore]) -> Value {
    json!(dist
        .iter()
        .map(|d| json!({"option": d.label, "score": d.score as f64 / 1000.0}))
        .collect::<Vec<_>>())
}

/// Turn a `choice-result` into the JSON this endpoint promises, gated on the
/// confidence threshold and the provider's own flat-distribution judgment.
fn shape_decision(result: &jev::ChoiceResult, threshold: u32) -> Value {
    if result.flat || result.confidence < threshold {
        return json!({
            "domain": "unknown",
            "decision": "disambiguate",
            "confidence": result.confidence as f64 / 1000.0,
            "distribution": distribution_json(&result.distribution),
        });
    }
    json!({
        "domain": result.selected,
        "decision": "route",
        "confidence": result.confidence as f64 / 1000.0,
    })
}

fn decision_error_message(e: DecisionError) -> String {
    match e {
        DecisionError::InvalidRequest(m) => format!("invalid request: {m}"),
        DecisionError::ProviderDenied(m) => format!("provider denied: {m}"),
        DecisionError::ProviderUnavailable(m) => format!("provider unavailable: {m}"),
        DecisionError::BadResponse(m) => format!("bad response: {m}"),
    }
}

async fn route_intent(request: Request) -> Outcome {
    let req: RouteReq = match parse(request).await {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if let Err(m) = validate_query(&req.query) {
        return Outcome::Bad(m);
    }

    let choice_req = ChoiceRequest {
        state: req.query,
        instructions: ROUTING_INSTRUCTIONS.to_string(),
        options: ROUTABLE_DOMAINS.iter().map(|s| s.to_string()).collect(),
    };
    match jev::choose(&choice_req) {
        Ok(result) => {
            Outcome::Json(200, shape_decision(&result, confidence_threshold()).to_string())
        }
        Err(e) => Outcome::Err(503, format!("jev decision failed: {}", decision_error_message(e))),
    }
}

// ---- helpers ---------------------------------------------------------------------

async fn parse<T: for<'a> Deserialize<'a>>(request: Request) -> Result<T, String> {
    let body = read_body(request).await.map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

const MAX_BODY_BYTES: usize = 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    const JSON: &str = "application/json";
    match result {
        Outcome::Json(code, body) => respond(code, JSON, body),
        Outcome::Bad(msg) => respond(400, JSON, json!({ "error": msg }).to_string()),
        Outcome::Err(code, msg) => respond(code, JSON, json!({ "error": msg }).to_string()),
        Outcome::NotFound => respond(404, JSON, b"{\"error\":\"not_found\"}".to_vec()),
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use bindings::jev::decision::decision::OptionScore;

    fn result(selected: &str, confidence: u32, flat: bool) -> jev::ChoiceResult {
        jev::ChoiceResult {
            selected: selected.to_string(),
            confidence,
            distribution: vec![OptionScore { label: selected.to_string(), score: confidence }],
            flat,
        }
    }

    #[test]
    fn a_query_must_be_one_to_a_thousand_characters() {
        assert!(validate_query("route me").is_ok());
        assert!(validate_query("").is_err(), "empty");
        assert!(validate_query(&"x".repeat(1001)).is_err(), "too long");
        assert!(validate_query(&"x".repeat(1000)).is_ok(), "exactly the limit");
    }

    #[test]
    fn high_confidence_routes_decisively() {
        let out = shape_decision(&result("helpdesk-domain", 940, false), 650);
        assert_eq!(out["domain"], "helpdesk-domain");
        assert_eq!(out["decision"], "route");
        assert_eq!(out["confidence"], 0.94);
    }

    #[test]
    fn confidence_below_threshold_falls_back_to_disambiguate() {
        let out = shape_decision(&result("helpdesk-domain", 600, false), 650);
        assert_eq!(out["domain"], "unknown");
        assert_eq!(out["decision"], "disambiguate");
    }

    #[test]
    fn confidence_at_exactly_the_threshold_routes() {
        let out = shape_decision(&result("helpdesk-domain", 650, false), 650);
        assert_eq!(out["decision"], "route");
    }

    #[test]
    fn a_flat_distribution_forces_disambiguate_regardless_of_confidence() {
        // High confidence number, but the provider itself says the distribution
        // carries no signal — that judgment overrides the number.
        let out = shape_decision(&result("helpdesk-domain", 900, true), 650);
        assert_eq!(out["domain"], "unknown");
        assert_eq!(out["decision"], "disambiguate");
    }

    #[test]
    fn disambiguate_carries_the_full_distribution() {
        let mut r = result("helpdesk-domain", 500, true);
        r.distribution = vec![
            OptionScore { label: "helpdesk-domain".into(), score: 500 },
            OptionScore { label: "clinic-domain".into(), score: 500 },
        ];
        let out = shape_decision(&r, 650);
        assert_eq!(out["distribution"].as_array().unwrap().len(), 2);
    }
}
