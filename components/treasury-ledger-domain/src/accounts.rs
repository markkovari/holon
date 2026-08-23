//! `accounts` — see CONTRACT.md.
//!
//! The one hard property: `POST /api/accounts/{id}/credit` is read-add-write against an
//! optimistically-locked record. `records::update` refuses when the revision has moved since
//! we read it — that refusal means "someone else wrote first, read again", not "give up and
//! tell the caller". So credit retries: re-read, recompute the sum from what's there NOW, and
//! write again with the fresh revision, bounded so a truly stuck store still answers.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

fn authorize(route: &Route, target: &str, action: &str) -> Result<authz::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = authz::Permission { target: target.to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(p) => Ok(p),
        Err(authz::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(authz::AuthError::BackendUnavailable(_)) | Err(authz::AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match seg.as_slice() {
        ["api", "accounts"] if matches!(method, Method::Post) => create_account(route, body),
        ["api", "accounts", id, "credit"] if matches!(method, Method::Post) => {
            credit(route, id, body)
        }
        ["api", "accounts", id] if matches!(method, Method::Get) => get_account(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn create_account(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize(route, "accounts", "write") {
        return r;
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let name = req.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let currency = req.get("currency").and_then(Value::as_str).unwrap_or("").to_string();
    if name.is_empty() || currency.is_empty() {
        return Reply::err(400, "invalid_account");
    }
    let amount = match req.get("start").and_then(Value::as_str) {
        Some(s) => match money::parse(s, &currency) {
            Ok(a) => a,
            Err(_) => return Reply::err(400, "bad_money"),
        },
        // No `start` given: default to this currency's zero. There's no direct
        // "is this currency known" call, so validate it via `format`, which fails on an
        // unknown currency exactly like `parse` would.
        None => {
            let zero = money::Amount { units: 0, currency: currency.clone() };
            if money::format(&zero).is_err() {
                return Reply::err(400, "bad_money");
            }
            zero
        }
    };
    let doc = json!({
        "name": name, "currency": currency, "units": amount.units,
        "opened_at": rfc3339(now_secs()),
    });
    match records::create("accounts", &doc.to_string(), &["name".to_string()]) {
        Ok(e) => Reply::json(201, json!({ "id": e.id })),
        Err(_) => Reply::err(500, "create_failed"),
    }
}

fn get_account(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize(route, "accounts", "read") {
        return r;
    }
    match records::get("accounts", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

const MAX_CREDIT_ATTEMPTS: u32 = 20;

fn credit(route: &Route, id: &str, body: &str) -> Reply {
    if let Err(r) = authorize(route, "accounts", "write") {
        return r;
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let amount_str = req.get("amount").and_then(Value::as_str).unwrap_or("");

    let first = match records::get("accounts", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut doc: Value = match serde_json::from_str(&first.data) {
        Ok(v) => v,
        Err(_) => return Reply::err(500, "corrupt_account"),
    };
    let currency = doc.get("currency").and_then(Value::as_str).unwrap_or("").to_string();

    let credit_amount = match money::parse(amount_str, &currency) {
        Ok(a) if a.units > 0 => a,
        _ => return Reply::err(400, "invalid_amount"),
    };

    let mut revision = first.revision;
    let mut units = doc.get("units").and_then(Value::as_i64).unwrap_or(0);

    for _ in 0..MAX_CREDIT_ATTEMPTS {
        let current = money::Amount { units, currency: currency.clone() };
        let new_amount = match money::add(&current, &credit_amount) {
            Ok(a) => a,
            Err(_) => return Reply::err(500, "money_error"),
        };
        if let Some(o) = doc.as_object_mut() {
            o.insert("units".into(), json!(new_amount.units));
        }
        match records::update("accounts", id, &doc.to_string(), revision) {
            Ok(_) => return Reply::json(200, json!({ "units": new_amount.units })),
            Err(records::StoreError::RevisionConflict(_)) => {
                // Someone else wrote first. Read again, recompute from what's actually
                // there now, and retry — this is not a failure to report to the caller.
                match records::get("accounts", id) {
                    Ok(fresh) => {
                        revision = fresh.revision;
                        doc = serde_json::from_str(&fresh.data).unwrap_or(doc);
                        units = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
                        continue;
                    }
                    Err(_) => return Reply::err(503, "contended"),
                }
            }
            Err(records::StoreError::NotFound) => return Reply::err(404, "not_found"),
            Err(_) => return Reply::err(503, "contended"),
        }
    }
    Reply::err(503, "contended")
}
