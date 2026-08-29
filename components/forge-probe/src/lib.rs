//! `forge-probe` — an instrument for `git:forge` (see wit/probe.wit).
//!
//!   GET /base                  what the base branch points at
//!   POST /propose              body is {branch, title, body, message, changes:[{path,content}]}
//!
//! Errors answer 200 with `{"error":…}`, because which of the four they are is
//! the interesting output: `conflict` (the branch exists) and `not-configured`
//! (no token) want opposite fixes and a status code flattens them together.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::git::forge::repo as forge;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

fn err(e: forge::ForgeError) -> String {
    let (kind, msg) = match e {
        forge::ForgeError::Rejected(m) => ("rejected", m),
        forge::ForgeError::Unavailable(m) => ("unavailable", m),
        forge::ForgeError::NotConfigured(m) => ("not-configured", m),
        forge::ForgeError::Conflict(m) => ("conflict", m),
    };
    serde_json::json!({ "error": kind, "detail": msg }).to_string()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = match (request.method(), route.as_str()) {
            (Method::Get, "/base") => match forge::base_commit("") {
                Ok(sha) => serde_json::json!({ "base": sha }).to_string(),
                Err(e) => err(e),
            },
            (Method::Post, "/propose") => {
                let raw = read_body(&request);
                let v: serde_json::Value = match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        let m =
                            serde_json::json!({ "error": "bad-request", "detail": e.to_string() });
                        respond(response_out, &m.to_string());
                        return;
                    }
                };
                let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                let changes: Vec<forge::FileChange> = v["changes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| forge::FileChange {
                        path: c["path"].as_str().unwrap_or_default().to_string(),
                        content: c["content"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect();
                let p = forge::Proposal {
                    branch: s("branch"),
                    base: s("base"),
                    title: s("title"),
                    body: s("body"),
                    message: s("message"),
                    changes,
                };
                match forge::propose(&p) {
                    Ok(o) => serde_json::json!({
                        "number": o.number, "url": o.url,
                        "commit": o.commit, "branch": o.branch,
                    })
                    .to_string(),
                    Err(e) => err(e),
                }
            }
            _ => serde_json::json!({ "service": "forge-probe", "routes": ["/base", "/propose"] })
                .to_string(),
        };
        respond(response_out, &body);
    }
}

fn respond(response_out: ResponseOutparam, body: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(200);
    let out = resp.body().expect("body");
    ResponseOutparam::set(response_out, Ok(resp));
    if let Ok(stream) = out.write() {
        for chunk in body.as_bytes().chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
        drop(stream);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
