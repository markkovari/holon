//! `tickets` — what can be answered at all.
//!
//! A delivery address nothing can deliver to is refused HERE: accepted, it would become a
//! ticket that drafts a reply, spends the budget, enqueues, and dead-letters days later for
//! a reason nobody upstream can act on.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

/// Resolve the bearer against `{tickets, action}`, mapped per CONTRACT.md's table.
fn require(route: &Route, action: &str) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required =
        auth_types::Permission { target: "tickets".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(principal) => Ok(principal),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_))
        | Err(auth_types::AuthError::Internal(_)) => Err(Reply::err(503, "auth_unavailable")),
        // invalid-token, expired, malformed, and anything else we didn't enumerate:
        // all read as "not authenticated" per the contract table.
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

/// Put the store's minted id onto the record's JSON, so `{"id": …, …ticket…}` is one value.
fn with_id(id: String, data: &str) -> Value {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut map) = v {
        map.insert("id".to_string(), json!(id));
    }
    v
}

fn create(route: &Route, body: &str) -> Reply {
    if let Err(r) = require(route, "write") {
        return r;
    }
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "invalid_ticket"),
    };
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("").trim();
    let text = req.get("body").and_then(Value::as_str).unwrap_or("").trim();
    let customer = req.get("customer").and_then(Value::as_str).unwrap_or("").trim();
    // The one rule with its own check: a delivery address nothing can deliver to is
    // refused here, not dead-lettered days later for a reason nobody can act on.
    if subject.is_empty()
        || text.is_empty()
        || customer.is_empty()
        || !customer.starts_with("webhook:")
    {
        return Reply::err(400, "invalid_ticket");
    }
    let data = json!({
        "subject": subject,
        "body": text,
        "customer": customer,
        "state": "open",
        "opened_at": rfc3339(now_secs()),
    })
    .to_string();
    match records::create("tickets", &data, &["state".to_string(), "customer".to_string()]) {
        Ok(entry) => Reply::json(201, json!({ "id": entry.id })),
        Err(_) => Reply::err(503, "store_unavailable"),
    }
}

fn get_one(route: &Route, id: &str) -> Reply {
    if let Err(r) = require(route, "read") {
        return r;
    }
    match records::get("tickets", id) {
        Ok(entry) => Reply::json(200, with_id(entry.id.clone(), &entry.data)),
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn list(route: &Route) -> Reply {
    if let Err(r) = require(route, "read") {
        return r;
    }
    let state = {
        let s = route.param("state");
        if s.is_empty() {
            "open".to_string()
        } else {
            s
        }
    };
    let limit = {
        let s = route.param("limit");
        if s.is_empty() {
            20
        } else {
            s.parse::<u32>().unwrap_or(20)
        }
    }
    .min(100);

    // `find_by` wants the value JSON-encoded, not the bare string — `"\"open\""`, not
    // `open`. Passing the bare string is a well-formed call that just never matches
    // anything: a wrong query returns `Ok(vec![])`, which reads exactly like a desk with
    // nothing waiting on it.
    let encoded = json!(state).to_string();
    match records::find_by("tickets", "state", &encoded) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.id.cmp(&b.id)); // ids are sortable ULIDs: oldest first
            let tickets: Vec<Value> = entries
                .into_iter()
                .take(limit as usize)
                .map(|e| with_id(e.id.clone(), &e.data))
                .collect();
            Reply::json(200, json!({ "tickets": tickets }))
        }
        Err(_) => Reply::err(503, "store_unavailable"),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "tickets"]) => create(route, body),
        (Method::Get, ["api", "tickets", id]) => get_one(route, id),
        (Method::Get, ["api", "tickets"]) => list(route),
        _ => Reply::err(404, "not_found"),
    }
}
