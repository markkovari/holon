//! `treasury-ledger-domain` — concurrent transfers that never lose money.
//!
//! ROUTER, and no part may write it. See CONTRACT.md.

mod accounts;
#[allow(warnings)]
mod bindings;
mod reconcile;
mod transfers;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::money::amount::arithmetic as money;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();
guestio::guest_bearer!();

struct Component;

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

pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    pub bearer: String,
    /// The `Idempotency-Key` header, empty when absent.
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

/// A `wasi:config` value, with a default.
pub fn cfg(key: &str, default: &str) -> String {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn now_secs() -> u64 {
    wall_clock::now().seconds
}

use guestfmt::rfc3339;

fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("treasurer").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec![
            "accounts:write".into(),
            "accounts:read".into(),
            "transfers:write".into(),
            "transfers:read".into(),
        ],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "treasury".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Two accounts, each opened with a starting balance the caller names.
///
/// Scaffold: `transfers` and `reconcile` both need accounts and neither may wait for
/// `accounts` to exist. The starting balance is a parameter because a contention test has to
/// choose it — a fixture with a fixed one would make the interesting cases unreachable.
fn seed(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let start = req.get("start").and_then(Value::as_str).unwrap_or("100.00");
    let currency = req.get("currency").and_then(Value::as_str).unwrap_or("EUR");
    let amount = match money::parse(start, currency) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_money"),
    };
    let mut ids = Vec::new();
    for name in ["left", "right"] {
        let doc = json!({
            "name": name, "currency": currency, "units": amount.units,
            "opened_at": rfc3339(now_secs()),
        });
        match records::create("accounts", &doc.to_string(), &["name".to_string()]) {
            Ok(e) => ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    Reply::json(201, json!({ "account_ids": ids, "units": amount.units }))
}

/// One journal line, written straight through.
///
/// Scaffold. `reconcile` recomputes balances FROM the journal, and it is judged with
/// `transfers` — the only part that writes one — still a stub. Without this it could only ever
/// be judged against an empty journal, which is the one case where a part that reads nothing
/// looks correct.
fn seed_journal(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let f = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let units = req.get("units").and_then(Value::as_i64).unwrap_or(0);
    if f("from").is_empty() || f("to").is_empty() || units <= 0 {
        return Reply::err(400, "invalid_line");
    }
    let doc = json!({
        "transfer": req.get("transfer").and_then(Value::as_str).unwrap_or("fixture"),
        "from": f("from"), "to": f("to"), "units": units, "at": rfc3339(now_secs()),
    });
    match records::create("journal", &doc.to_string(), &["from".to_string(), "to".to_string()]) {
        Ok(e) => Reply::json(201, json!({ "line": e.id })),
        Err(_) => Reply::err(503, "store_unavailable"),
    }
}

/// Every journal line, straight out of the store.
///
/// Scaffold. `transfers` ends in a journal write and the route that READS the journal belongs to
/// `reconcile` — a stub while `transfers` is judged. Without this, the one thing that proves a
/// transfer was written down is unobservable exactly when it matters, and the part is
/// unpassable. That is not a hypothetical: it cost app 6 its first run, three generations of six
/// branches each, on a check no implementation could satisfy.
fn peek_journal() -> Reply {
    let mut lines = Vec::new();
    let mut after = String::new();
    loop {
        let Ok(page) = records::list_records("journal", 200, &after) else { break };
        let empty = page.entries.is_empty();
        for e in &page.entries {
            if let Ok(mut v) = serde_json::from_str::<Value>(&e.data) {
                if let Some(o) = v.as_object_mut() {
                    o.insert("id".into(), json!(e.id));
                }
                lines.push(v);
            }
        }
        if empty || page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    lines.sort_by(|a, b| {
        a.get("at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("at").and_then(Value::as_str).unwrap_or(""))
    });
    Reply::json(200, json!({ "lines": lines }))
}

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
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn header(request: &IncomingRequest, name: &str) -> String {
    request
        .headers()
        .get(name)
        .first()
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer: bearer(&request).unwrap_or_default(),
            idempotency_key: header(&request, "idempotency-key"),
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "token"] => mint(&body),
            ["test", "seed"] => seed(&body),
            ["test", "journal"] if matches!(method, Method::Post) => seed_journal(&body),
            ["test", "journal"] => peek_journal(),
            // The stored record, straight out of the store, so a part is judgeable on what it
            // WROTE without depending on the part that owns the read route.
            ["test", "account", id] => match records::get("accounts", id) {
                Ok(e) => {
                    let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
                    if let Some(o) = v.as_object_mut() {
                        o.insert("id".into(), json!(e.id));
                        o.insert("revision".into(), json!(e.revision));
                    }
                    Reply::json(200, v)
                }
                Err(_) => Reply::err(404, "not_found"),
            },
            ["api", "transfers", ..] => transfers::handle(&method, &route, &body),
            ["api", "reconcile", ..] | ["api", "journal", ..] => {
                reconcile::handle(&method, &route, &body)
            }
            ["api", "accounts", ..] => accounts::handle(&method, &route, &body),
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
