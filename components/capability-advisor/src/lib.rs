//! `capability-advisor` — confirms capsearch's lexical hits before a goal
//! trusts them, over HTTP so native code (`goalrun`) can call it.
//!
//! ADR-0089's own admission: term-overlap retrieval over-matches, since many
//! component descriptions are tautological ("`x` — reference implementation
//! of `x:y`") and match nothing a caller would type. This does not replace
//! that retrieval (`capsearch` stays cheap, deterministic, first) — it asks
//! ONE narrow, atomic question per candidate, batched into a SINGLE
//! `jev:decision::evaluate` call: "does this specific capability plausibly
//! satisfy what this goal needs?" A Jev failure never blocks a run — it
//! degrades to `"unavailable": true`, and the caller falls back to trusting
//! capsearch's own ranking unfiltered, exactly as it does today.
//!
//!   POST /evaluate
//!     {"goal": "...", "candidates": [{"id": "...", "name": "...", "description": "..."}]}
//!   ->
//!     {"confirmed": [{"name": "...", "probability": 0.92}],
//!      "rejected":  [{"name": "...", "probability": 0.10}],
//!      "unavailable": false}

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../jev-decision/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "comp:advisor/capability-advisor",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde::Deserialize;
use serde_json::json;

use bindings::jev::decision::decision::{self as jev, GateCriteria, Question, QuestionKind};
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

struct Component;

/// Milli-probability, 0..=1000. At or above this, a candidate counts as
/// confirmed rather than rejected.
const CONFIRM_THRESHOLD: u32 = 500;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").trim_matches('/');
        let result = match (&request.get_method(), route) {
            (Method::Post, "evaluate") => evaluate_route(request).await,
            (Method::Get, "health") => Outcome::Json(200, json!({"status": "ok"}).to_string()),
            _ => Outcome::NotFound,
        };
        emit(result)
    }
}

enum Outcome {
    Json(u16, String),
    Bad(String),
    NotFound,
}

#[derive(Deserialize)]
struct Candidate {
    id: String,
    name: String,
    description: String,
}

#[derive(Deserialize)]
struct EvaluateReq {
    goal: String,
    candidates: Vec<Candidate>,
}

/// The Noul question one candidate becomes — atomic, per System One's own
/// guidance against broad composite judgments.
fn question_for(c: &Candidate) -> Question {
    Question {
        id: c.id.clone(),
        instructions: format!(
            "Does the capability '{}' — {} — plausibly satisfy what this goal needs?",
            c.name, c.description
        ),
        kind: QuestionKind::Gate(GateCriteria {
            true_hint: "The capability's description covers what the goal is asking for.".into(),
            false_hint: "The capability is unrelated, or only superficially similar in wording."
                .into(),
        }),
    }
}

async fn evaluate_route(request: Request) -> Outcome {
    let req: EvaluateReq = match parse(request).await {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.candidates.is_empty() {
        return Outcome::Bad("no candidates given".into());
    }

    let questions: Vec<Question> = req.candidates.iter().map(question_for).collect();
    let by_id: std::collections::HashMap<&str, &Candidate> =
        req.candidates.iter().map(|c| (c.id.as_str(), c)).collect();

    match jev::evaluate(&req.goal, &questions) {
        Err(_) => Outcome::Json(
            200,
            json!({"confirmed": [], "rejected": [], "unavailable": true}).to_string(),
        ),
        Ok(answers) => {
            let mut confirmed = Vec::new();
            let mut rejected = Vec::new();
            for a in answers {
                let Some(c) = by_id.get(a.id.as_str()) else { continue };
                let probability = match a.outcome {
                    Ok(bindings::jev::decision::decision::AnswerKind::Gate(g)) => g.probability,
                    // A per-question failure (the provider left this id out,
                    // or answered with the wrong shape) is not a confirmation
                    // — reject rather than trust it by default.
                    _ => 0,
                };
                let entry = json!({"name": c.name, "probability": probability as f64 / 1000.0});
                if probability >= CONFIRM_THRESHOLD {
                    confirmed.push(entry);
                } else {
                    rejected.push(entry);
                }
            }
            Outcome::Json(
                200,
                json!({"confirmed": confirmed, "rejected": rejected, "unavailable": false})
                    .to_string(),
            )
        }
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
    match result {
        Outcome::Json(code, body) => respond(code, "application/json", body),
        Outcome::Bad(msg) => respond(400, "application/json", json!({ "error": msg }).to_string()),
        Outcome::NotFound => respond(404, "application/json", "{\"error\":\"not_found\"}"),
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_names_the_capability_and_asks_one_atomic_thing() {
        let c = Candidate {
            id: "1".into(),
            name: "auth-guard".into(),
            description: "password hashing and JWT sessions".into(),
        };
        let q = question_for(&c);
        assert_eq!(q.id, "1");
        assert!(q.instructions.contains("auth-guard"));
        assert!(q.instructions.contains("password hashing"));
        assert!(matches!(q.kind, QuestionKind::Gate(_)));
    }
}
