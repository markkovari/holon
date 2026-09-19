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
mod bindings;

use serde::Deserialize;
use serde_json::json;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::jev::decision::decision::{self as jev, GateCriteria, Question, QuestionKind};
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

/// Milli-probability, 0..=1000. At or above this, a candidate counts as
/// confirmed rather than rejected.
const CONFIRM_THRESHOLD: u32 = 500;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").trim_matches('/');
        let result = match (&request.method(), route) {
            (Method::Post, "evaluate") => evaluate_route(&request),
            (Method::Get, "health") => Outcome::Json(200, json!({"status": "ok"}).to_string()),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
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

fn evaluate_route(request: &IncomingRequest) -> Outcome {
    let req: EvaluateReq = match parse(request) {
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

fn parse<T: for<'a> Deserialize<'a>>(request: &IncomingRequest) -> Result<T, String> {
    let body = read_body(request).map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

const MAX_BODY_BYTES: usize = 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Bad(msg) => respond(response_out, 400, json!({ "error": msg }).to_string().as_bytes()),
        Outcome::NotFound => respond(response_out, 404, b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
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
