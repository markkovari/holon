//! `llm-probe` — an instrument for `llm:inference` (see wit/probe.wit).
//!
//!   GET /chat?q=…    one user message, the model's reply
//!   GET /describe    what the provider says it is
//!
//! Errors come back as JSON with a 200, because the four `infer-error` cases are
//! the interesting output here: `provider-denied` (the key was wrong),
//! `provider-unavailable` (the host refused the egress, or nothing was
//! listening) and `bad-response` want completely different fixes, and a status
//! code flattens all three into "it didn't work".

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::llm::inference::inference as llm;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn err(e: llm::InferError) -> String {
    let (kind, msg) = match e {
        llm::InferError::InvalidRequest(m) => ("invalid-request", m),
        llm::InferError::ProviderDenied(m) => ("provider-denied", m),
        llm::InferError::ProviderUnavailable(m) => ("provider-unavailable", m),
        llm::InferError::BadResponse(m) => ("bad-response", m),
        llm::InferError::NoContent => ("no-content", String::new()),
    };
    format!("{{\"error\":\"{kind}\",\"detail\":\"{}\"}}", esc(&msg))
}

/// The SEED is the interesting knob here. It exists in the contract for
/// reproducibility, and a swarm uses it for the same reason in reverse: N
/// branches asking one question with N seeds is how they explore differently
/// while staying replayable.
fn options(seed: u64) -> llm::Options {
    llm::Options {
        model: String::new(),
        temperature: 0,
        max_tokens: 0,
        stop: Vec::new(),
        seed,
    }
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };

        let body = match (request.method(), route.as_str()) {
            (Method::Get, "/chat") => {
                let msg = llm::Message {
                    role: llm::Role::User,
                    content: param(&query, "q"),
                };
                let seed = param(&query, "seed").parse().unwrap_or(0);
                match llm::chat(&[msg], &options(seed)) {
                    Ok(c) => format!(
                        "{{\"text\":\"{}\",\"model\":\"{}\",\"finish\":\"{}\"}}",
                        esc(&c.text),
                        esc(&c.model),
                        esc(&c.finish_reason)
                    ),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/describe") => {
                let (name, streaming) = llm::describe();
                format!("{{\"provider\":\"{}\",\"streaming\":{streaming}}}", esc(&name))
            }
            _ => "{\"service\":\"llm-probe\",\"routes\":[\"/chat?q=\",\"/describe\"]}".to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = stream.blocking_write_and_flush(body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
