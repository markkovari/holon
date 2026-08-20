//! `posting` — the only irreversible step, and it happens once.
//!
//! `POST .../post` is guarded twice: `idempotency:guard` makes a retried request replay
//! its first answer instead of posting again, and `ledger:doubleentry` refuses an entry
//! that does not balance before anything is stored.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::ledger::doubleentry::ledger;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg, now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

fn authorize(route: &Route, action: &str) -> Result<(), Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = Permission { target: "invoices".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(_) => Ok(()),
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(AuthError::BackendUnavailable(_)) | Err(AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        // invalid-token, expired, malformed, unknown-tenant, and anything else the
        // guard reports for a bearer it did not accept — all read as "not authenticated".
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

/// `POST /api/invoices/{id}/post` — posting is the only irreversible thing this app
/// does, so it happens once: `begin` reserves the key or replays a prior answer verbatim,
/// and the ledger gets to refuse the entry before anything is stored.
fn post_invoice(id: &str, route: &Route) -> Reply {
    if let Err(r) = authorize(route, "post") {
        return r;
    }

    let key = route.idempotency_key.clone();
    if key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }

    let ttl: u64 = cfg("idempotency-ttl-secs", "86400").parse().unwrap_or(86400);
    match idem::begin(&key, ttl) {
        Ok(Some(cached)) => {
            // A retry, not a fresh posting: replay exactly what the first call answered.
            let body: Value = serde_json::from_slice(&cached.body).unwrap_or(Value::Null);
            return Reply::json(cached.status, body);
        }
        Ok(None) => {}
        Err(idem::IdemError::InProgress) => return Reply::err(409, "in_progress"),
        Err(idem::IdemError::BackendUnavailable(_)) => {
            return Reply::err(503, "idempotency_unavailable");
        }
    }

    let stored = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut inv: Value = match serde_json::from_str(&stored.data) {
        Ok(v) => v,
        Err(_) => return Reply::err(404, "not_found"),
    };

    if inv.get("state").and_then(Value::as_str) == Some("posted") {
        return Reply::err(409, "already_posted");
    }
    let lines = inv.get("lines").and_then(Value::as_array).cloned().unwrap_or_default();
    if lines.is_empty() {
        return Reply::err(409, "nothing_to_post");
    }
    let total_units = inv.get("total_units").and_then(Value::as_i64).unwrap_or(0);

    let receivable = cfg("receivable-account", "assets:receivable");
    let revenue = cfg("revenue-account", "revenue:services");
    let entry = ledger::Entry {
        id: id.to_string(),
        memo: id.to_string(),
        lines: vec![
            ledger::Line { account: receivable.clone(), amount: total_units, side: ledger::Side::Debit },
            ledger::Line { account: revenue.clone(), amount: total_units, side: ledger::Side::Credit },
        ],
    };
    if let Err(err) = ledger::validate(&entry) {
        // The ledger refused; an unbalanced entry posted anyway is how a ledger stops
        // being one, so nothing below this line runs.
        return match err {
            ledger::LedgerError::Unbalanced((debits, credits)) => {
                Reply::json(500, json!({ "error": "unbalanced", "debits": debits, "credits": credits }))
            }
            _ => Reply::err(500, "unbalanced"),
        };
    }

    let posted_at = rfc3339(now_secs());
    inv["state"] = json!("posted");
    inv["entry"] = json!({
        "id": id,
        "posted_at": posted_at,
        "lines": [
            { "account": receivable, "amount": total_units, "side": "debit" },
            { "account": revenue, "amount": total_units, "side": "credit" },
        ],
    });
    if records::update("invoices", id, &inv.to_string(), stored.revision).is_err() {
        return Reply::err(500, "post_failed");
    }

    let status = 201u16;
    let body = json!({ "entry": id, "total_units": total_units, "posted_at": posted_at });
    // Record the answer BEFORE returning it: a posting that succeeds without `complete`
    // is a posting that will happen again on the caller's next retry.
    let _ = idem::complete(&key, status, body.to_string().as_bytes());
    Reply::json(status, body)
}

/// `GET /api/invoices/{id}/entry` — the stored entry, or `not_posted` when there is none.
fn get_entry(id: &str, route: &Route) -> Reply {
    if let Err(r) = authorize(route, "read") {
        return r;
    }
    let inv: Value = match records::get("invoices", id) {
        Ok(e) => serde_json::from_str(&e.data).unwrap_or(Value::Null),
        Err(_) => return Reply::err(404, "not_posted"),
    };
    match inv.get("entry") {
        Some(entry) if !entry.is_null() => Reply::json(200, entry.clone()),
        _ => Reply::err(404, "not_posted"),
    }
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let id = route.segments.get(2).cloned().unwrap_or_default();
    match (method, route.segments.last().map(String::as_str)) {
        (Method::Post, Some("post")) => post_invoice(&id, route),
        (Method::Get, Some("entry")) => get_entry(&id, route),
        _ => Reply::err(404, "not_found"),
    }
}