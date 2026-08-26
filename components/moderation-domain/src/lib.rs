//! `moderation-domain` — content in, a decision out, and the model does not have the
//! last word.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `intake`,
//! `verdict` and `queue`, answers `/health`, mints a test token, seeds a queue, and
//! writes one policy rule directly. Three parts need it and none owns it.
//!
//! `src/intake.rs`, `src/verdict.rs` and `src/queue.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why these three
//!
//! The chain is about PRECEDENCE. `queue` writes the rules, `verdict` is the only part
//! that asks a model and the only one that may overrule it, and `intake` decides what
//! ever gets that far. A part that lets the model win, or that reports an outcome
//! without saying what overruled what, passes every request-shaped check and leaves an
//! app nobody can govern or audit.

#[allow(warnings)]
mod bindings;
mod intake;
mod queue;
mod verdict;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::event::bus::bus;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::policy::guard::guard as policy;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();

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

fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `wasi:config` value, with a default.
///
/// Scaffold: reading config is plumbing every part would otherwise write out, and the
/// contract names the keys. What a part does with the value is the goal.
pub fn cfg(key: &str, default: &str) -> String {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Unix seconds, for anything that has to be stamped.
pub fn now_secs() -> u64 {
    wall_clock::now().seconds
}

/// RFC3339 UTC seconds — what the contract stores in `reported_at`/`assisted_at`.
///
/// Written out by hand because this component has no date library and does not need
/// one: the epoch-to-civil conversion is twenty lines and a dependency is a decision.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    // Howard Hinnant's civil_from_days, the shift-to-March algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

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
        None => {
            vec!["items:write".into(), "items:read".into(), "items:moderate".into()]
        }
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "moderation".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Two pending items, so `verdict` and `queue` can be judged before `intake` exists.
///
/// Scaffold, and it says what it is: `decision` is absent on purpose, because producing
/// one is `verdict`'s job and a fixture that pre-filled it would let a part that decides
/// nothing pass. One item carries a link and one does not — the only fact the contract's
/// `has_link` attribute is about, and therefore the only way a rule can be seen to fire
/// on one and not the other.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (text, author) in [
        ("come and see the pictures at https://gallery.example/album", "ada"),
        ("thanks for organising this, it was genuinely useful", "bo"),
    ] {
        match records::create(
            "items",
            &json!({
                "text": text, "author": author, "state": "pending",
                "submitted_at": rfc3339(now_secs()),
            })
            .to_string(),
            &["state".to_string(), "author".to_string()],
        ) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "item_ids": ids }))
}

/// One rule, written straight through `policy:guard`.
///
/// `verdict` is judged with `queue` stubbed, and its whole reason for existing is what a
/// matching rule does to a model's opinion — without a rule it could only ever be judged
/// on the case where the policy stays silent. Deny anything whose text carries a link,
/// which is a fact about the item and not about the model, so the rule fires whatever the
/// model happens to say.
fn seed_rules() -> Reply {
    let rule = policy::Rule {
        id: "no-links".to_string(),
        action: "publish".to_string(),
        effect: policy::Effect::Deny,
        conditions: vec![policy::Condition {
            left: "resource.has_link".to_string(),
            op: policy::Op::Eq,
            right: "true".to_string(),
        }],
        priority: 10,
    };
    match policy::set_rules(&cfg("policy-domain", "moderation"), &[rule]) {
        Ok(()) => Reply::json(201, json!({ "rules": 1 })),
        Err(_) => Reply::err(503, "policy_unavailable"),
    }
}

/// What has been published, straight off the bus.
///
/// Scaffold. `verdict`'s whole job ends in a publish, and the route that reads the bus
/// belongs to `queue` — which is a stub while `verdict` is judged. Without this, the one
/// thing proving a decision left the system is unobservable exactly when it matters.
///
/// A different consumer group from the contract's, so a gate reading here never competes
/// with `queue`'s own reads.
fn peek_bus() -> Reply {
    match bus::poll("moderation.decided", "fixture-reader", 50) {
        Ok(events) => Reply::json(
            200,
            json!({
                "events": events
                    .iter()
                    .map(|e| json!({
                        "id": e.id,
                        "at": e.at,
                        "payload": serde_json::from_slice::<Value>(&e.payload)
                            .unwrap_or(json!(null)),
                    }))
                    .collect::<Vec<_>>()
            }),
        ),
        Err(_) => Reply::err(503, "bus_unavailable"),
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
        let bearer = header(&request, "authorization")
            .strip_prefix("Bearer ")
            .unwrap_or_default()
            .to_string();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer,
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
            ["test", "seed"] => seed(),
            ["test", "rules"] => seed_rules(),
            ["test", "events"] => peek_bus(),
            // The stored document, straight out of the store. Scaffold, and it says
            // what it is: a part must be judgeable on what it WROTE without
            // depending on the part that owns the route for reading it back.
            ["test", "item", id] => match records::get("items", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before the `api/items` arm: a match on ["api","items",..] would hand the
            // review route to `intake` instead.
            ["api", "items", _, "review"] => verdict::handle(&method, &route, &body),
            ["api", "items", ..] => intake::handle(&method, &route, &body),
            ["api", "rules"] | ["api", "queue"] | ["api", "events"] => {
                queue::handle(&method, &route, &body)
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
