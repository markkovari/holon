//! `reconcile` — see CONTRACT.md.
//!
//! The auditor: it does not trust stored balances. For every opened account it walks the
//! WHOLE journal (paged — `list_records` does not hand back everything in one call) and
//! recomputes opening + credits - debits with `money`'s exact arithmetic, then compares that
//! against what the account currently holds. Same idempotency key -> same report, verbatim,
//! forever (the idempotency-guard is the source of truth for that, not a HashMap here).

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

fn authorize(route: &Route, action: &str) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = auth_types::Permission { target: "transfers".into(), action: action.into() };
    match authz::authorize(&route.bearer, &required) {
        Ok(p) => Ok(p),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_))
        | Err(auth_types::AuthError::Internal(_)) => Err(Reply::err(503, "auth_unavailable")),
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

/// The whole journal, oldest-write-order, PAGED — a reconciliation that stops at the first
/// page agrees with the books right up to the point they disagree.
fn read_journal() -> Result<Vec<Value>, ()> {
    let mut lines = Vec::new();
    let mut after = String::new();
    loop {
        let page = records::list_records("journal", 200, &after).map_err(|_| ())?;
        let empty = page.entries.is_empty();
        for e in &page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                lines.push(v);
            }
        }
        if empty || page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    Ok(lines)
}

fn compute_reconcile(body: &str) -> Result<Value, &'static str> {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let opened = req.get("opened").and_then(Value::as_array).cloned().unwrap_or_default();
    let lines = read_journal().map_err(|_| "store_unavailable")?;

    let mut drift = Vec::new();
    let mut checked = 0u64;
    for o in &opened {
        let account_id = o.get("account").and_then(Value::as_str).unwrap_or("").to_string();
        if account_id.is_empty() {
            continue;
        }
        let opening_units = o.get("units").and_then(Value::as_i64).unwrap_or(0);

        let entry = match records::get("accounts", &account_id) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let acct: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let currency = acct.get("currency").and_then(Value::as_str).unwrap_or("EUR").to_string();
        let actual_units = acct.get("units").and_then(Value::as_i64).unwrap_or(0);
        checked += 1;

        let mut expected = money::Amount { units: opening_units, currency: currency.clone() };
        for line in &lines {
            let units = line.get("units").and_then(Value::as_i64).unwrap_or(0);
            let delta = money::Amount { units, currency: currency.clone() };
            if line.get("to").and_then(Value::as_str) == Some(account_id.as_str()) {
                if let Ok(a) = money::add(&expected, &delta) {
                    expected = a;
                }
            } else if line.get("from").and_then(Value::as_str) == Some(account_id.as_str()) {
                if let Ok(a) = money::subtract(&expected, &delta) {
                    expected = a;
                }
            }
        }

        let actual = money::Amount { units: actual_units, currency };
        if let Ok(c) = money::compare(&expected, &actual) {
            if c != 0 {
                drift.push(json!({
                    "account": account_id,
                    "expected": expected.units,
                    "actual": actual.units,
                    "delta": actual.units - expected.units,
                }));
            }
        }
    }

    Ok(json!({
        "checked": checked,
        "drift": drift,
        "balanced": drift.is_empty(),
        "journal_lines": lines.len(),
    }))
}

fn reconcile(route: &Route, body: &str) -> Reply {
    if let Err(e) = authorize(route, "read") {
        return e;
    }
    if route.idempotency_key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }

    match idem::begin(&route.idempotency_key, 3600) {
        Ok(Some(cached)) => {
            let v: Value = serde_json::from_slice(&cached.body).unwrap_or(json!({}));
            return Reply::json(cached.status, v);
        }
        Ok(None) => {}
        Err(idem::IdemError::InProgress) => return Reply::err(409, "in_progress"),
        Err(idem::IdemError::BackendUnavailable(_)) => {
            return Reply::err(503, "idempotency_unavailable")
        }
    }

    let resp = match compute_reconcile(body) {
        Ok(v) => v,
        Err(code) => {
            let _ = idem::forget(&route.idempotency_key);
            return Reply::err(503, code);
        }
    };

    let _ = idem::complete(&route.idempotency_key, 200, resp.to_string().as_bytes());
    Reply::json(200, resp)
}

fn journal(route: &Route) -> Reply {
    if let Err(e) = authorize(route, "read") {
        return e;
    }
    let limit_param = route.param("limit");
    let limit: usize = if limit_param.is_empty() {
        50
    } else {
        limit_param.parse().unwrap_or(50)
    };
    let limit = limit.clamp(1, 500);

    let mut lines = match read_journal() {
        Ok(l) => l,
        Err(_) => return Reply::err(503, "store_unavailable"),
    };
    lines.sort_by(|a, b| {
        a.get("at").and_then(Value::as_str).unwrap_or("").cmp(
            b.get("at").and_then(Value::as_str).unwrap_or(""),
        )
    });
    lines.truncate(limit);
    Reply::json(200, json!({ "lines": lines }))
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reconcile"]) => reconcile(route, body),
        (Method::Get, ["api", "journal"]) => journal(route),
        _ => Reply::err(404, "not_found"),
    }
}