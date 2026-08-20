//! `invoices` — opening an invoice, and deciding what currency its arithmetic will be
//! done in.
//!
//! A currency nobody can add up is an invoice that cannot be totalled, and finding
//! that out at posting time is finding it out too late — so it is checked here,
//! against `money:amount` itself, not against a list of codes written in this file.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::ratelimit::guard::limiter as rl;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

/// Map an `auth-error` onto the status/body every `/api/*` route agrees on.
fn auth_reply(e: AuthError) -> Reply {
    match e {
        AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
        AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
            Reply::err(503, "auth_unavailable")
        }
        // invalid-token / expired / malformed, and everything else this table
        // doesn't name — none of which this app's routes should ever produce.
        _ => Reply::err(401, "unauthenticated"),
    }
}

fn authorize(route: &Route, action: &str) -> Result<crate::bindings::auth::identity::types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = Permission { target: "invoices".to_string(), action: action.to_string() };
    authz::authorize(&route.bearer, &required).map_err(auth_reply)
}

/// Whether `money:amount` will accept this currency at all — asked of it directly,
/// by parsing a zero written with the right number of decimal places. `parse` wants
/// EXACTLY the currency's decimal count and calls anything else `unknown-currency`,
/// so a single "0.00" guess would wrongly reject a 0- or 3-decimal currency; try the
/// plausible widths instead of assuming EUR's two.
fn currency_ok(currency: &str) -> bool {
    for decimals in 0..=4u32 {
        let zero = if decimals == 0 {
            "0".to_string()
        } else {
            format!("0.{}", "0".repeat(decimals as usize))
        };
        if money::parse(&zero, currency).is_ok() {
            return true;
        }
    }
    false
}

fn create(route: &Route, body: &str) -> Reply {
    let principal = match authorize(route, "write") {
        Ok(p) => p,
        Err(r) => return r,
    };

    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "invalid_invoice"),
    };
    let customer = req.get("customer").and_then(Value::as_str).unwrap_or("");
    let currency = req.get("currency").and_then(Value::as_str).unwrap_or("");
    if customer.is_empty() || currency.is_empty() {
        return Reply::err(400, "invalid_invoice");
    }
    if !currency_ok(currency) {
        return Reply::err(400, "bad_money");
    }

    // The throttle takes two calls: `check` only asks, `record_failure` is what
    // counts. Keyed on the subject, not the tenant or the route — the same
    // caller is the same limit no matter which invoice they open.
    let key = &principal.subject;
    match rl::check(key) {
        Ok(_) => {}
        Err(rl::LimitError::Locked(secs)) => {
            return Reply::json(429, json!({ "error": "rate_limited", "retry_after": secs }))
        }
        Err(rl::LimitError::BackendUnavailable(_)) => {
            return Reply::err(503, "rate_limit_unavailable")
        }
    }

    let doc = json!({
        "customer": customer,
        "currency": currency,
        "state": "draft",
        "created_at": rfc3339(now_secs()),
        "lines": [],
        "total_units": 0,
    });
    let entry = match records::create(
        "invoices",
        &doc.to_string(),
        &["state".to_string(), "customer".to_string()],
    ) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_failed"),
    };

    // Only now, once the invoice is actually accepted, does this count against
    // the window.
    let _ = rl::record_failure(key);

    Reply::json(201, json!({ "id": entry.id }))
}

fn get_invoice(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize(route, "read") {
        return r;
    }
    match records::get("invoices", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
            if let Value::Object(map) = &mut v {
                map.insert("id".to_string(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "invoices"]) => create(route, body),
        (Method::Get, ["api", "invoices", id]) => get_invoice(route, id),
        _ => Reply::err(404, "not_found"),
    }
}