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
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Post, ["api", "intent"]) => classify_intent(&request),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
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

fn classify_intent(request: &IncomingRequest) -> Outcome {
    let req: IntentReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };

    if req.query.is_empty() || req.query.len() > 1000 {
        return Outcome::Bad("query must be between 1 and 1000 characters".into());
    }

    let system_prompt = "You are a semantic routing AI for the Holon system.
Classify the user's natural language query into exactly ONE of the following backend domains:
- helpdesk-domain: for IT support, ticketing, or customer service.
- clinic-domain: for medical, vet, or appointment scheduling.
- studio-domain: for booking studios, music practice rooms, or creative spaces.
- grocery-domain: for ordering food, groceries, or eshop items.
- unknown: if the query doesn't match any of the above.

Respond with ONLY a JSON object in this exact format:
{\"domain\": \"<domain-name>\", \"confidence\": <0.0-1.0>}

Do not include markdown blocks or any other text.";

    let opts = Options {
        model: "".into(),
        temperature: 0,
        max_tokens: 50,
        stop: vec![],
        seed: 42,
    };

    match llm::complete(&req.query, system_prompt, &opts) {
        Ok(completion) => {
            // The LLM should return a raw JSON string like `{"domain": "helpdesk-domain", "confidence": 0.95}`
            match serde_json::from_str::<Value>(&completion.text) {
                Ok(json_val) => Outcome::Json(200, json_val.to_string()),
                Err(_) => {
                    // Fallback if LLM output was weird
                    let fallback = json!({"domain": "unknown", "confidence": 0.0, "raw": completion.text});
                    Outcome::Json(200, fallback.to_string())
                }
            }
        }
        Err(e) => {
            Outcome::Err(503, format!("LLM inference failed: {:?}", e))
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
