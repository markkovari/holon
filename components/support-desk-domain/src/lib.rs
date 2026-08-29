//! `support-desk-domain` — a model writes the reply, and the reply gets there.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `tickets`, `reply`
//! and `courier`, answers `/health`, mints a token, opens a session with its CSRF token,
//! seeds tickets, and can put a reply straight into the outbox. Three parts need it and
//! none owns it.
//!
//! `src/tickets.rs`, `src/reply.rs` and `src/courier.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why these three
//!
//! The chain is about DELIVERY. `tickets` decides what can be answered, `reply` is the
//! only part that spends a model call and it must ENQUEUE rather than send, and `courier`
//! is the only part that talks to the far end. A part that sends inline loses a reply the
//! moment the far end is down; a courier that acks a refusal loses it silently. Neither
//! failure is visible in a request that succeeds, which is why the gates run a sink they
//! can break on purpose.

#[allow(warnings)]
mod bindings;
mod courier;
mod reply;
mod tickets;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::outbox::dispatch::queue as outbox;
use bindings::records::store::store as records;
use bindings::session::store::store as sessions;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();
guestio::guest_bearer!();

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
    /// The agent's session id (`x-session`) and its CSRF token (`x-csrf`), empty when
    /// absent. Handed over raw: verifying them is `session:store`'s job, and doing it here
    /// would take the part's reason for importing that capability away.
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

/// A `wasi:config` value as a number, with a default.
///
/// Scaffold: reading config is plumbing every part would otherwise write out, and the
/// contract names the keys. What a part does with the number is the goal.
pub fn cfg_u64(key: &str, default: u64) -> u64 {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Unix seconds, for anything that has to be stamped.
pub fn now_secs() -> u64 {
    wall_clock::now().seconds
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
    // The tenant is the budget's subject, so a gate has to be able to choose it: two
    // tenants sharing one budget would make an exhausted-budget check untestable.
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

/// Two open tickets aimed at a target the caller names, so `reply` and `courier` can be
/// judged before `tickets` exists.
///
/// The target is a parameter because a gate has to point it at a sink it controls — a
/// fixture with a hardcoded address would make delivery unobservable, which is the one
/// thing this app is about.
///
/// Scaffold, and it says what it is: `reply` is absent on purpose, because drafting one is
/// the reply part's job and a fixture that pre-filled it would let a part that drafts
/// nothing pass.
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

/// One reply put straight into the outbox, in the contract's payload shape.
///
/// `courier` is judged with `reply` stubbed, and it has nothing to deliver otherwise. The
/// shape here is the contract's, which is what makes a part that invented its own shape
/// pass its gate and fail the composition.
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

/// A session, and the CSRF token that goes with it.
///
/// `reply`'s first check is the CSRF one, and the session it checks against belongs to no
/// part. Opening one here is what lets `reply` be judged on anything past that check.
fn open_session() -> Reply {
    match sessions::create(b"{}", 900) {
        Ok(s) => Reply::json(201, json!({ "session": s.id, "csrf": s.csrf_token })),
        Err(_) => Reply::err(503, "session_unavailable"),
    }
}

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a JSON
            // document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One header, as a string. Absent, repeated or non-UTF8 all read as empty.
fn header(request: &IncomingRequest, name: &str) -> String {
    let fields = request.headers();
    let values = fields.get(name);
    values.first().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
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
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        // The router: `/health`, the token and the fixture here, everything else to
        // the part that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "token"] => mint(&body),
            ["test", "seed"] => seed(&body),
            ["test", "session"] => open_session(),
            ["test", "enqueue"] => enqueue_fixture(&body),
            // The stored document, straight out of the store. Scaffold, and it says
            // what it is: a part must be judgeable on what it WROTE without
            // depending on the part that owns the route for reading it back.
            ["test", "ticket", id] => match records::get("tickets", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before the `api/tickets` arm: a match on ["api","tickets",..] would hand
            // the reply route to `tickets` instead.
            ["api", "tickets", _, "reply"] => reply::handle(&method, &route, &body),
            ["api", "tickets", ..] => tickets::handle(&method, &route, &body),
            ["api", "deliver"] | ["api", "dead-letters", ..] => {
                courier::handle(&method, &route, &body)
            }
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            if !payload.is_null() {
                let _ = write_all(&stream, payload.to_string().as_bytes());
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
