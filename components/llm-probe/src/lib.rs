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
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::llm::inference::inference as llm;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
/// This bit a real run — a 4573-byte contract — and cost four failed starts to
/// find, so it is written the same way everywhere now.
///
/// Not a flat 4096-byte loop: `check-write` is the stream saying how much it will
/// take right now, usually far more, so this writes in whatever bites it offers,
/// waits on the pollable when it offers none, and flushes ONCE at the end.
///
/// Returns false when the stream is gone. For an SSE loop that means the client
/// hung up, which is ordinary and not an error.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

fn read_body(request: IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a
            // JSON document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

        let method = request.method();
        // Read the body BEFORE matching: `consume` takes the request, so it cannot
        // happen inside an arm that also needs the method.
        let posted = match method {
            Method::Post => read_body(request),
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
            _ => "{\"service\":\"llm-probe\",\"routes\":[\"/chat?q=\",\"POST /chat\",\"/describe\"]}".to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
