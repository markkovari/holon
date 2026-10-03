//! `triage-assist-domain` — authenticated, rate-limited defect intake with an AI
//! severity assist and an audit ledger, as one component.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `intake`,
//! `assist` and `ledger`, answers `/health` so the harness can tell "the component
//! is not up" from "the component is wrong", mints a test token, and seeds a
//! fixture. Three parts need it and none owns it, which is the shape a shared file
//! has to have when three agents work at once.
//!
//! `src/intake.rs`, `src/assist.rs` and `src/ledger.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why these three
//!
//! They form a chain, not a set: `assist` reads what `intake` STORED rather than
//! what the request said, because a redaction that happens after the model call is
//! a leak nothing in the response reveals — and `ledger` is the only way to show,
//! afterwards, that any of it happened. A part that invents its own storage shape
//! or its own audit signature passes its own gate and fails the composition.

mod assist;
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../llm-inference/wit",
            "../ai-inference/wit",
            "../pii-redact/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "triage:assist/triage-assist-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod intake;
mod ledger;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();
guestio::guest_p3_bearer!();

struct Component;

/// What a handler answers with: a status and a JSON body.
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
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

/// One request, as a part sees it.
///
/// The bearer is handed over as a STRING and not as a principal: resolving it is
/// `auth:identity/authorizer`'s job and doing it here would take the part's whole
/// reason for importing that capability away.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    /// The `Authorization: Bearer …` value, empty when the header is absent.
    pub bearer: String,
    /// The trace this request belongs to — the `traceparent` header's trace-id, or
    /// one generated here. Every `ledger::note` in this request carries it.
    pub trace: String,
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

/// Unix seconds, for anything that has to be stamped.
pub fn now_secs() -> u64 {
    system_clock::now().seconds as u64
}

use guestfmt::rfc3339;

/// A token for a test caller, so no gate has to log in through a part it is not
/// judging.
///
/// Scaffold, and it is `session::issue` rather than a hand-built JWT for the same
/// reason the parts are made to call `authorize`: a fixture that mints its own
/// tokens is a fixture that can drift from what the verifier accepts, and then
/// every part fails for the router's reason.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec!["reports:write".into(), "reports:read".into()],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "triage-assist".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Two reports written straight to the store, so a part can be judged before the
/// part upstream of it exists.
///
/// `assist` needs reports and must not depend on `intake` being finished; `ledger`
/// needs neither but is cheaper to drive with something in the store. So the
/// fixture writes the contract's document shape directly.
///
/// Scaffold, and it says what it is: `assist` is absent on purpose, because
/// producing it is the assist part's job and a fixture that pre-filled it would let
/// a part that never calls a model pass.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, body, component) in [
        // Already masked, because `intake` is what masks and this bypasses it: a
        // fixture holding a live address would make `assist`'s gate a PII test of
        // the router.
        (
            "Checkout button invisible on Safari",
            "reached me at [EMAIL] — the button renders white on white after the promo banner loads",
            "web",
        ),
        ("Login fails, silently", "no error is shown to the user", "auth"),
    ] {
        match records::create(
            "reports",
            &json!({
                "title": title,
                "body": body,
                "component": component,
                "state": "open",
                "reporter": "fixture",
                "reported_at": rfc3339(now_secs()),
            })
            .to_string(),
            &["component".to_string(), "state".to_string()],
        ) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "report_ids": ids }))
}

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

/// One header, as a string. Absent, repeated or non-UTF8 all read as empty.
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
        // `traceparent` is `00-<32hex trace>-<16hex span>-<2hex flags>`; anything
        // else, including absent, gets a trace of its own so no request is
        // unattributable in the ledger.
        let trace = match header(&request, "traceparent").split('-').nth(1) {
            Some(t) if t.len() == 32 => t.to_string(),
            _ => format!("{:032x}", now_secs()),
        };
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer,
            trace,
        };
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(request).await,
            _ => String::new(),
        };

        // The router: `/health`, the token and the fixture here, everything else to
        // the part that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "token"] => mint(&body),
            ["test", "seed"] => seed(),
            // The stored document, straight out of the store. Scaffold, and it says
            // what it is: a part must be judgeable on what it WROTE without
            // depending on the part that owns the route for reading it back.
            ["test", "report", id] => match records::get("reports", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before the `api/reports` arm: a match on ["api","reports",..] would
            // hand the assist route to `intake` instead.
            ["api", "reports", _, "assist"] => assist::handle(&method, &route, &body),
            ["api", "reports", ..] => intake::handle(&method, &route, &body),
            ["api", "audit", ..] => ledger::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        // Every dispatched request is noted, which is what lets `ledger` be judged
        // before either other part exists. The parts note their own events too, with
        // the detail only they know; this line is the traffic, not the story.
        if !seg.is_empty() && seg[0] == "api" {
            ledger::note(
                &route.trace,
                "http.request",
                if status < 400 { "ok" } else { "error" },
                "router",
                &format!("{} /{} -> {}", method_name(&method), seg.join("/"), status),
            );
        }

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let bytes = if payload.is_null() { Vec::new() } else { payload.to_string().into_bytes() };
        respond_with(status, headers, bytes)
    }
}

fn method_name(m: &Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        _ => "?",
    }
}

bindings::export!(Component with_types_in bindings);
