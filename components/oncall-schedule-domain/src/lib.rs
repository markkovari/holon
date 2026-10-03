//! `oncall-schedule-domain` — router (scaffold, not part of the goal).
//!
//! `src/handlers.rs` is the goal: nothing in it is implemented yet. This file
//! answers `/health` and mints a test token at `POST /test/token`
//! `{"subject","roles":[...],"scopes":[...]}` — a fixture, so a gate never has
//! to drive a real register/login flow to test `handlers.rs` in isolation.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../notify-dispatch/wit",
            "wit",
        ],
        world: "oncall:schedule/oncall-schedule-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
    }
}
mod handlers;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::p3::handler::Guest;
use bindings::p3::clocks::system_clock;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use serde_json::{json, Value};

pub const TENANT: &str = "oncallschedule";

guestio::guest_p3_respond!();
guestio::guest_p3_bearer!();

struct Component;

/// What a handler answers with: a status and a JSON body.
pub struct Reply {
    pub status: u16,
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

/// One request, as `handlers::handle` sees it.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    /// The `Authorization: Bearer …` value, empty when the header is absent.
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

pub fn now_secs() -> u64 {
    system_clock::now().seconds as u64
}

/// A test token, minted directly (never a real register/login) — the ROUTER's
/// fixture, never `handlers.rs`'s: a fixture that mints its own tokens can
/// drift from what the verifier actually accepts, and then `handlers.rs` fails
/// for the router's reason, not its own.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("test").to_string();
    let roles: Vec<String> = req
        .get("roles")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let scopes: Vec<String> = req
        .get("scopes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let principal = auth_types::Principal {
        subject: subject.clone(),
        tenant: TENANT.to_string(),
        roles,
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({"token": pair.access_token, "subject": subject})),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

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
            ["test", "token"] => mint(&body),
            ["api", ..] => handlers::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let body = if payload.is_null() { String::new() } else { payload.to_string() };
        respond(status, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
