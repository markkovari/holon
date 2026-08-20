//! `copilot` — the model names the lines; it does not do the arithmetic.
//!
//! The model (`ai:inference`) is asked for words only. Every number — the split of the
//! total into shares — comes from `money::allocate`. See CONTRACT.md Part 2.

use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(_method: &Method, route: &Route, body: &str) -> Reply {
    let id = match route.segments.get(2) {
        Some(id) => id.clone(),
        None => return Reply::err(404, "not_found"),
    };

    // --- identity: one call resolves the bearer AND checks the permission ---
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthenticated");
    }
    let required = Permission { target: "invoices".into(), action: "write".into() };
    if let Err(e) = authz::authorize(&route.bearer, &required) {
        use crate::bindings::auth::identity::types::AuthError;
        return match e {
            AuthError::InvalidToken(_) | AuthError::Expired | AuthError::Malformed(_) => {
                Reply::err(401, "unauthenticated")
            }
            AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
            AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                Reply::err(503, "auth_unavailable")
            }
            _ => Reply::err(401, "unauthenticated"),
        };
    }

    // --- parse + validate the request body ---
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "invalid_suggestion"),
    };
    let prose = req.get("prose").and_then(Value::as_str).unwrap_or("");
    let total_str = req.get("total").and_then(Value::as_str).unwrap_or("");
    let shares = req.get("shares").and_then(Value::as_u64).unwrap_or(0);
    if prose.is_empty() || total_str.is_empty() || !(2..=12).contains(&shares) {
        return Reply::err(400, "invalid_suggestion");
    }
    let shares = shares as u32;

    // --- the invoice must exist and be a draft ---
    let entry = match records::get("invoices", &id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut invoice: Value = match serde_json::from_str(&entry.data) {
        Ok(v) => v,
        Err(_) => return Reply::err(500, "internal"),
    };
    let state = invoice.get("state").and_then(Value::as_str).unwrap_or("");
    if state != "draft" {
        return Reply::err(409, "already_posted");
    }
    let currency = invoice.get("currency").and_then(Value::as_str).unwrap_or("").to_string();

    // --- parse the total before spending a model call ---
    let total = match money::parse(total_str, &currency) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_money"),
    };

    // --- the model names the lines; it produces text, nothing else ---
    let prompt = format!(
        "Write exactly {shares} short invoice line descriptions (one per line, no numbering) \
         for this work:\n{prose}"
    );
    let text = match ai::generate(&prompt, "") {
        Ok(t) => t,
        Err(_) => return Reply::err(503, "suggest_unavailable"),
    };
    let mut descriptions: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .take(shares as usize)
        .collect();
    while descriptions.len() < shares as usize {
        descriptions.push(format!("Line {}", descriptions.len() + 1));
    }

    // --- money::allocate produces every number; the model never does ---
    let shares_amounts = match money::allocate(&total, shares) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_money"),
    };

    let lines: Vec<Value> = shares_amounts
        .iter()
        .zip(descriptions.iter())
        .map(|(amount, memo)| json!({ "memo": memo, "units": amount.units }))
        .collect();

    let mut sum = shares_amounts[0].clone();
    for a in &shares_amounts[1..] {
        sum = match money::add(&sum, a) {
            Ok(s) => s,
            Err(_) => return Reply::err(400, "bad_money"),
        };
    }
    if sum.units != total.units {
        // money::allocate always sums to the total; a mismatch means it wasn't used.
        return Reply::err(500, "internal");
    }

    invoice["lines"] = Value::Array(lines.clone());
    invoice["total_units"] = json!(sum.units);
    if records::update("invoices", &id, &invoice.to_string(), entry.revision).is_err() {
        return Reply::err(500, "internal");
    }

    let total_display = match money::format(&total) {
        Ok(s) => s,
        Err(_) => return Reply::err(400, "bad_money"),
    };
    Reply::json(200, json!({ "lines": lines, "total_units": sum.units, "total": total_display }))
}