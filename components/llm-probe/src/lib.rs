//! `llm-probe` — an instrument for `llm:inference` (see wit/probe.wit).
//!
//!   GET  /chat?q=…    one user message in the query string, the model's reply
//!   POST /chat?seed=  the same, with the message as the BODY
//!   GET  /describe    what the provider says it is
//!
//! The POST exists because a prompt outgrew a URL: the contract-negotiation call
//! (ADR-0086) carries a whole interface definition and a candidate's failures, and
//! a query string is not where that belongs.
//!
//! Errors come back as JSON with a 200, because the four `infer-error` cases are
//! the interesting output here: `provider-denied` (the key was wrong),
//! `provider-unavailable` (the host refused the egress, or nothing was
//! listening) and `bad-response` want completely different fixes, and a status
//! code flattens all three into "it didn't work".

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../llm-inference/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "comp:llmprobe/llm-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::llm::inference::inference as llm;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

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
    llm::Options { model: String::new(), temperature: 0, max_tokens: 0, stop: Vec::new(), seed }
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };

        let method = request.get_method();
        // Read the body BEFORE matching: `consume` takes the request, so it cannot
        // happen inside an arm that also needs the method.
        let posted = match method {
            Method::Post => read_body(request).await,
            _ => String::new(),
        };

        let body = match (method, route.as_str()) {
            (Method::Post, "/chat") | (Method::Get, "/chat") => {
                let content = if posted.is_empty() { param(&query, "q") } else { posted };
                let msg = llm::Message { role: llm::Role::User, content };
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
            _ => {
                "{\"service\":\"llm-probe\",\"routes\":[\"/chat?q=\",\"POST /chat\",\"/describe\"]}"
                    .to_string()
            }
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
