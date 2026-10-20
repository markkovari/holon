//! `invoice-copilot-domain` — the model writes the words and the money component does the

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../money/wit",
            "../ledger/wit",
            "../idempotency-guard/wit",
            "../llm-inference/wit",
            "../ai-inference/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "invoice:copilot/invoice-copilot-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod copilot;
mod invoices;
mod posting;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::money::amount::arithmetic as money;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::records::store::store as records;
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
    /// Raw: a part inventing a key from the body builds a different guarantee than the caller asked for.
    pub idempotency_key: String,
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

pub fn cfg(key: &str, default: &str) -> String {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn now_secs() -> u64 {
    system_clock::now().seconds as u64
}

use guestfmt::rfc3339;

/// Via `session::issue`: a hand-built JWT could drift from what the verifier accepts.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec!["invoices:write".into(), "invoices:read".into(), "invoices:post".into()],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "acme".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// The second's amounts come from `money::allocate`: hand division could give `posting` a bad total.
fn seed() -> Reply {
    let mut ids = Vec::new();
    let empty = json!({
        "customer": "acme-gmbh", "currency": "EUR", "state": "draft",
        "created_at": rfc3339(now_secs()), "lines": [], "total_units": 0,
    });
    match records::create(
        "invoices",
        &empty.to_string(),
        &["state".to_string(), "customer".to_string()],
    ) {
        Ok(e) => ids.push(e.id),
        Err(_) => return Reply::err(500, "seed_failed"),
    }

    let total = match money::parse("100.00", "EUR") {
        Ok(t) => t,
        Err(_) => return Reply::err(500, "seed_money_failed"),
    };
    let shares = match money::allocate(&total, 3) {
        Ok(s) => s,
        Err(_) => return Reply::err(500, "seed_money_failed"),
    };
    let memos = ["Discovery workshop, day one", "Discovery workshop, day two", "Written summary"];
    let lines: Vec<Value> = shares
        .iter()
        .zip(memos)
        .map(|(a, memo)| json!({ "memo": memo, "units": a.units }))
        .collect();
    let filled = json!({
        "customer": "acme-gmbh", "currency": "EUR", "state": "draft",
        "created_at": rfc3339(now_secs()),
        "lines": lines,
        "total_units": shares.iter().map(|a| a.units).sum::<i64>(),
    });
    match records::create(
        "invoices",
        &filled.to_string(),
        &["state".to_string(), "customer".to_string()],
    ) {
        Ok(e) => ids.push(e.id),
        Err(_) => return Reply::err(500, "seed_failed"),
    }
    Reply::json(201, json!({ "invoice_ids": ids }))
}

/// Past this the read gives up and the body reads as empty.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

/// Absent, repeated or non-UTF-8 all read as empty.
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
            idempotency_key: header(&request, "idempotency-key"),
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
            ["test", "seed"] => seed(),

            // Scaffold: a part is judged on what it wrote.
            ["test", "invoice", id] => match records::get("invoices", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before the `api/invoices` arm, which would take the suggest and post routes.
            ["api", "invoices", _, "lines", "suggest"] => copilot::handle(&method, &route, &body),
            ["api", "invoices", _, "post"] | ["api", "invoices", _, "entry"] => {
                posting::handle(&method, &route, &body)
            }
            ["api", "invoices", ..] => invoices::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let body = if payload.is_null() { String::new() } else { payload.to_string() };
        respond(status, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
