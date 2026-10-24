//! `support-desk-domain` — a model writes the reply, and the reply gets there.
//! the reply when the far end is down, invisibly in a request that succeeds.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../session-store/wit",
            "../quota/wit",
            "../outbox/wit",
            "../notify-dispatch/wit",
            "../llm-inference/wit",
            "../ai-inference/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "support:desk/support-desk-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod courier;
mod reply;
mod tickets;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::outbox::dispatch::queue as outbox;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::records::store::store as records;
use bindings::session::store::store as sessions;
use serde_json::{json, Value};

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

struct Component;

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all — see `no_content`.
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

/// The bearer stays a STRING: resolving it is `auth:identity/authorizer`'s job.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    pub bearer: String,
    /// Raw `x-session` / `x-csrf`: verifying them is `session:store`'s job.
    pub session: String,
    pub csrf: String,
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

use guestfmt::rfc3339;

/// The fixture mints only where a gate or demo asks for it (`test-tokens = on`): composed into a
/// real app, an open `/test/token` would hand anyone a token with any roles.
fn test_tokens() -> bool {
    matches!(bindings::wasi::config::store::get("test-tokens"), Ok(Some(v)) if v == "on")
}

/// `session::issue`, not a hand-built JWT, so the fixture cannot drift from the verifier.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada").to_string();
    // A gate must choose the tenant: two tenants sharing one budget make exhaustion untestable.
    let tenant = req.get("tenant").and_then(Value::as_str).unwrap_or("acme").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec![
            "tickets:write".into(),
            "tickets:read".into(),
            "tickets:reply".into(),
            "tickets:deliver".into(),
        ],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: tenant.clone(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Lets `reply` and `courier` be judged before `tickets` exists; the target is a parameter so a
/// gate can point it at a sink it controls.
fn seed(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let target = req
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("webhook:http://127.0.0.1:1/hook")
        .to_string();
    let mut ids = Vec::new();
    for (subject, text) in [
        ("Invoice does not match my plan", "I am on the team plan and was charged for pro."),
        ("Cannot add a second seat", "The seats page shows one row and no add button."),
    ] {
        match records::create(
            "tickets",
            &json!({
                "subject": subject, "body": text, "customer": target,
                "state": "open", "opened_at": rfc3339(now_secs()),
            })
            .to_string(),
            &["state".to_string(), "customer".to_string()],
        ) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "ticket_ids": ids }))
}

/// In the contract's payload shape, so `courier` can be judged with `reply` stubbed.
fn enqueue_fixture(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let target =
        req.get("target").and_then(Value::as_str).unwrap_or("webhook:http://127.0.0.1:1/hook");
    let payload = json!({
        "ticket": req.get("ticket").and_then(Value::as_str).unwrap_or("fixture"),
        "target": target,
        "subject": "Re: a fixture ticket",
        "body": req.get("body").and_then(Value::as_str).unwrap_or("a fixture reply"),
    });
    match outbox::enqueue("support.reply", payload.to_string().as_bytes(), 0) {
        Ok(id) => Reply::json(201, json!({ "event": id })),
        Err(_) => Reply::err(503, "outbox_unavailable"),
    }
}

/// `reply`'s first check is CSRF, against a session that belongs to no part.
fn open_session() -> Reply {
    match sessions::create(b"{}", 900) {
        Ok(s) => Reply::json(201, json!({ "session": s.id, "csrf": s.csrf_token })),
        Err(_) => Reply::err(503, "session_unavailable"),
    }
}

/// Past this the body reads as empty, instead of growing until the memory cap traps the component.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

fn header(request: &Request, name: &str) -> String {
    let fields = request.get_headers();
    let values = fields.get(name);
    values.first().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

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
            session: header(&request, "x-session"),
            csrf: header(&request, "x-csrf"),
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
            ["test", "seed"] => seed(&body),
            ["test", "session"] => open_session(),
            ["test", "enqueue"] => enqueue_fixture(&body),
            // Straight out of the store, so a part is judged on what it WROTE.
            ["test", "ticket", id] => match records::get("tickets", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before `api/tickets`, which would hand the reply route to `tickets`.
            ["api", "tickets", _, "reply"] => reply::handle(&method, &route, &body),
            ["api", "tickets", ..] => tickets::handle(&method, &route, &body),
            ["api", "deliver"] | ["api", "dead-letters", ..] => {
                courier::handle(&method, &route, &body)
            }
            _ => Reply::err(404, "not_found"),
        };

        let body = if payload.is_null() { Vec::new() } else { payload.to_string().into_bytes() };
        respond(status, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
