//! intent-router — AI-Driven intent routing gateway mapping natural language to Holon domains.
#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::llm::inference::inference::{self as llm, Options};
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let result = if is_classify_route(&request.method(), &path) {
            classify_intent(&request)
        } else {
            Outcome::NotFound
        };
        emit(response_out, result);
    }
}

/// Only `POST /api/intent` routes anywhere; everything else is 404. Split out
/// so the matching itself — not just the handler that needs a live request —
/// can be tested directly.
fn is_classify_route(method: &Method, path: &str) -> bool {
    let route = path.split('?').next().unwrap_or("/");
    matches!(method, Method::Post) && route.trim_matches('/') == "api/intent"
}

enum Outcome {
    Json(u16, String),
    Bad(String),
    Err(u16, String),
    NotFound,
}

#[derive(Deserialize)]
struct IntentReq {
    query: String,
}

/// The only domains the router will ever hand back. A model asked to name one
/// of five things sometimes names a sixth — inventing `"helpdesk"` instead of
/// `"helpdesk-domain"`, say — and a caller dispatching on that string deserves
/// "unknown" over a route to somewhere that was never offered.
const KNOWN_DOMAINS: &[&str] =
    &["helpdesk-domain", "clinic-domain", "studio-domain", "grocery-domain", "unknown"];

const SYSTEM_PROMPT: &str = "You are a semantic routing AI for the Holon system.
Classify the user's natural language query into exactly ONE of the following backend domains:
- helpdesk-domain: for IT support, ticketing, or customer service.
- clinic-domain: for medical, vet, or appointment scheduling.
- studio-domain: for booking studios, music practice rooms, or creative spaces.
- grocery-domain: for ordering food, groceries, or eshop items.
- unknown: if the query doesn't match any of the above.

Respond with ONLY a JSON object in this exact format:
{\"domain\": \"<domain-name>\", \"confidence\": <0.0-1.0>}

Do not include markdown blocks or any other text.";

/// 1 to 1000 characters — long enough for a real question, short enough that
/// a caller cannot turn this into a place to paste a document.
fn validate_query(query: &str) -> Result<(), String> {
    if query.is_empty() || query.len() > 1000 {
        return Err("query must be between 1 and 1000 characters".into());
    }
    Ok(())
}

/// Turn what the model said into the JSON this endpoint promises: a `domain`
/// from `KNOWN_DOMAINS` and a `confidence`. Three ways this can go wrong, and
/// each is handled rather than trusted:
///   - the reply isn't JSON at all (a model that added a sentence, markdown);
///   - it's JSON but not shaped like `{"domain": ..., "confidence": ...}`;
///   - it's shaped right but names a domain nobody offered it — a
///     hallucinated label is worse than "unknown" because a caller matching
///     on it would find no route rather than an honest miss.
/// All three land on the same fallback, carrying the raw text so a caller
/// debugging a bad classification can see what the model actually said.
fn shape_completion(text: &str) -> Value {
    let fallback = || json!({"domain": "unknown", "confidence": 0.0, "raw": text});
    let Ok(parsed) = serde_json::from_str::<Value>(text) else { return fallback() };
    let Some(domain) = parsed.get("domain").and_then(Value::as_str) else { return fallback() };
    if !KNOWN_DOMAINS.contains(&domain) {
        return fallback();
    }
    parsed
}

fn classify_intent(request: &IncomingRequest) -> Outcome {
    let req: IntentReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if let Err(m) = validate_query(&req.query) {
        return Outcome::Bad(m);
    }

    let opts = Options { model: "".into(), temperature: 0, max_tokens: 50, stop: vec![], seed: 42 };
    match llm::complete(&req.query, SYSTEM_PROMPT, &opts) {
        Ok(completion) => Outcome::Json(200, shape_completion(&completion.text).to_string()),
        Err(e) => Outcome::Err(503, format!("LLM inference failed: {:?}", e)),
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
        Outcome::Json(code, body) => respond(response_out, code, &[], body.as_bytes()),
        Outcome::Bad(msg) => {
            respond(response_out, 400, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Err(code, msg) => {
            respond(response_out, code, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::NotFound => respond(response_out, 404, &[], b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, extra: &[(&str, &str)], body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
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
    fn only_post_api_intent_routes() {
        assert!(is_classify_route(&Method::Post, "/api/intent"));
        assert!(is_classify_route(&Method::Post, "/api/intent?debug=1"));
        assert!(!is_classify_route(&Method::Get, "/api/intent"), "wrong method");
        assert!(!is_classify_route(&Method::Post, "/api/other"), "wrong path");
        assert!(!is_classify_route(&Method::Post, "/"), "root");
    }

    #[test]
    fn a_query_must_be_one_to_a_thousand_characters() {
        assert!(validate_query("route me").is_ok());
        assert!(validate_query("").is_err(), "empty");
        assert!(validate_query(&"x".repeat(1001)).is_err(), "too long");
        assert!(validate_query(&"x".repeat(1000)).is_ok(), "exactly the limit");
    }

    #[test]
    fn a_well_formed_known_domain_passes_through() {
        let out = shape_completion(r#"{"domain": "helpdesk-domain", "confidence": 0.92}"#);
        assert_eq!(out["domain"], "helpdesk-domain");
        assert_eq!(out["confidence"], 0.92);
    }

    /// A model asked to pick one of five things can still name a sixth. A
    /// caller matching on the string deserves "unknown" over a route to
    /// somewhere that was never offered.
    #[test]
    fn a_hallucinated_domain_falls_back_to_unknown() {
        let out = shape_completion(r#"{"domain": "billing-domain", "confidence": 0.8}"#);
        assert_eq!(out["domain"], "unknown");
        assert_eq!(out["raw"], r#"{"domain": "billing-domain", "confidence": 0.8}"#);
    }

    #[test]
    fn text_that_is_not_json_falls_back_to_unknown_with_the_raw_reply() {
        let out = shape_completion("I think this is a helpdesk question.");
        assert_eq!(out["domain"], "unknown");
        assert_eq!(out["confidence"], 0.0);
        assert_eq!(out["raw"], "I think this is a helpdesk question.");
    }

    #[test]
    fn json_missing_the_domain_field_falls_back_to_unknown() {
        let out = shape_completion(r#"{"confidence": 0.5}"#);
        assert_eq!(out["domain"], "unknown");
    }
}
