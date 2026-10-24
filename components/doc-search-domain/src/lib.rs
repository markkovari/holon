//! `doc-search-domain` — retrieval-augmented answers over an indexed library, behind

mod answer;
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../otp/wit",
            "../quota/wit",
            "../event-bus/wit",
            "../search-index/wit",
            "../cache/wit",
            "../llm-inference/wit",
            "../ai-inference/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "docsearch:agent/doc-search-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod library;
mod stepup;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::records::store::store as records;
use bindings::search::index::index as search;
use serde_json::{json, Value};

guestio::guest_p3_respond!();
guestio::guest_p3_bearer!();

struct Component;

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all.
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// A JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

/// A string, not a principal: resolving it is `auth:identity/authorizer`'s job, in the part.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    /// Empty when the header is absent.
    pub bearer: String,
}

impl Route {
    pub fn param(&self, key: &str) -> String {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| percent(v))
            .unwrap_or_default()
    }
}

use guestfmt::percent_decode as percent;

pub fn cfg_u64(key: &str, default: u64) -> u64 {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn now_secs() -> u64 {
    system_clock::now().seconds as u64
}

/// The fixture mints only where a gate or demo asks for it (`test-tokens = on`): composed into a
/// real app, an open `/test/token` would hand anyone a token with any roles.
fn test_tokens() -> bool {
    matches!(bindings::wasi::config::store::get("test-tokens"), Ok(Some(v)) if v == "on")
}

/// Via `session::issue`: a hand-built JWT could drift from what the verifier accepts.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec!["docs:write".into(), "docs:read".into()],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "docsearch".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Specific on purpose, so `answer`'s gate can tell a real answer from a canned one.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, text, tag) in [
        (
            "Deploying to the lattice",
            "A unit is placed by the reconciler, which polls inventory every three \
             seconds. A parked app with min zero is activated by the first request that \
             reaches it, so the first response after a deploy is slower than the rest.",
            "ops",
        ),
        (
            "The pooling allocator",
            "comp-host runs wasmtime with the pooling allocator, which reserves linear \
             memory up front. Raising the per-instance ceiling past sixty-four mebibytes \
             costs reserved address space per instance, not per request.",
            "runtime",
        ),
        (
            "Egress is default-deny",
            "A component reaches nothing outbound until an authority is granted by name. \
             Loopback and RFC1918 need a second opt-in, because on a lattice node those \
             are the bus and every other node.",
            "security",
        ),
    ] {
        let doc = json!({ "title": title, "text": text, "tag": tag });
        let entry = match records::create("docs", &doc.to_string(), &["tag".to_string()]) {
            Ok(e) => e,
            Err(_) => return Reply::err(500, "seed_failed"),
        };
        // Indexed as the contract says, or `answer`'s retrieval is untestable.
        if search::index_doc(&entry.id, &format!("{title}\n{text}"), &[tag.to_string()]).is_err() {
            return Reply::err(500, "seed_index_failed");
        }
        ids.push(entry.id);
    }
    Reply::json(201, json!({ "doc_ids": ids }))
}

/// `answer` is judged with `stepup` stubbed; without this it could only be judged on its 403.
fn mark_stepped_up(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada");
    let doc = json!({ "subject": subject, "verified_at": now_secs(), "secret": "fixture" });
    match records::create("stepups", &doc.to_string(), &["subject".to_string()]) {
        Ok(e) => Reply::json(201, json!({ "id": e.id, "subject": subject })),
        Err(_) => Reply::err(500, "stepup_failed"),
    }
}

/// Past this the body reads as empty, rather than the store's memory cap trapping the component.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer = bearer(&request).unwrap_or_default();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer,
        };
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(request).await,
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "token"] if test_tokens() => mint(&body),
            ["test", "seed"] => seed(),
            ["test", "stepup"] => mark_stepped_up(&body),
            // Scaffold: a part is judged on what it wrote, without the part owning the read route.
            ["test", "doc", id] => match records::get("docs", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            ["api", "answer"] => answer::handle(&method, &route, &body),
            ["api", "mfa", ..] => stepup::handle(&method, &route, &body),
            // `search` before the catch-all; `["api", "docs", ..]` already matches zero extra segments.
            ["api", "search"] | ["api", "docs", ..] => library::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let body = if payload.is_null() { String::new() } else { payload.to_string() };
        respond(status, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
