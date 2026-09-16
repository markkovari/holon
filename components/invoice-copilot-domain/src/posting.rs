use crate::{cfg, now_secs, Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::ledger::doubleentry::ledger;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "invoices", id, "post"]) => post_invoice(route, id),
        (Method::Get, ["api", "invoices", id, "entry"]) => get_entry(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "invoices".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => Reply::err(503, "auth_unavailable"),
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn post_invoice(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "post") {
        return r;
    }
    
    if route.idempotency_key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }
    
    let ttl_str = cfg("idempotency-ttl-secs", "86400");
    let ttl = ttl_str.parse::<u64>().unwrap_or(86400);
    match idem::begin(&route.idempotency_key, ttl) {
        Ok(Some(cached)) => {
            return Reply {
                status: cached.status,
                json: serde_json::from_slice(&cached.body).unwrap_or(json!({})),
            };
        }
        Ok(None) => {}
        Err(idem::IdemError::InProgress) => return Reply::err(409, "in_progress"),
        Err(idem::IdemError::BackendUnavailable(_)) => return Reply::err(503, "idempotency_unavailable"),
    }
    
    let entry = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let state = doc.get("state").and_then(Value::as_str).unwrap_or("");
    if state == "posted" {
        return Reply::err(409, "already_posted");
    } else if state != "draft" {
        return Reply::err(404, "not_found");
    }
    
    let lines = doc.get("lines").and_then(Value::as_array).unwrap_or(&vec![]).clone();
    if lines.is_empty() {
        return Reply::err(409, "nothing_to_post");
    }
    
    let total_units = doc.get("total_units").and_then(Value::as_i64).unwrap_or(0);
    
    let revenue_acc = cfg("revenue-account", "revenue:services");
    let receivable_acc = cfg("receivable-account", "assets:receivable");
    
    let ledger_entry = ledger::Entry {
        id: id.to_string(),
        memo: id.to_string(),
        lines: vec![
            ledger::Line {
                account: receivable_acc.clone(),
                amount: total_units,
                side: ledger::Side::Debit,
            },
            ledger::Line {
                account: revenue_acc.clone(),
                amount: total_units,
                side: ledger::Side::Credit,
            },
        ],
    };
    
    if let Err(ledger::LedgerError::Unbalanced((d, c))) = ledger::validate(&ledger_entry) {
        return Reply::json(500, json!({
            "error": "unbalanced",
            "debits": d,
            "credits": c
        }));
    } else if ledger::validate(&ledger_entry).is_err() {
        return Reply::err(500, "ledger_error");
    }
    
    let posted_at = guestfmt::rfc3339(now_secs());
    
    let entry_obj = json!({
        "id": id,
        "posted_at": posted_at,
        "lines": [
            { "account": receivable_acc, "amount": total_units, "side": "debit" },
            { "account": revenue_acc, "amount": total_units, "side": "credit" }
        ]
    });
    
    doc["state"] = json!("posted");
    doc["entry"] = entry_obj;
    
    if records::update("invoices", id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }
    
    let response_json = json!({
        "entry": id,
        "total_units": total_units,
        "posted_at": posted_at
    });
    
    let _ = idem::complete(&route.idempotency_key, 201, &serde_json::to_vec(&response_json).unwrap());
    
    Reply::json(201, response_json)
}

fn get_entry(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    match records::get("invoices", id) {
        Ok(e) => {
            let v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Some(entry) = v.get("entry") {
                Reply::json(200, entry.clone())
            } else {
                Reply::err(404, "not_posted")
            }
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}
