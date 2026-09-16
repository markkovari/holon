use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reconcile"]) => reconcile(route, body),
        (Method::Get, ["api", "journal"]) => list_journal(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "transfers".to_string(), action: action.to_string() };
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

fn reconcile(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    
    if route.idempotency_key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }
    
    let ttl = crate::cfg("idempotency-ttl-secs", "86400").parse::<u64>().unwrap_or(86400);
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
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let opened = req.get("opened").and_then(Value::as_array).unwrap_or(&vec![]).clone();
    
    let mut journal_lines = 0;
    let mut all_journal_lines = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records("journal", 200, &after) {
            Ok(p) => p,
            Err(_) => return Reply::err(503, "store_unavailable"),
        };
        let empty = page.entries.is_empty();
        for e in page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                all_journal_lines.push(v);
                journal_lines += 1;
            }
        }
        if empty || page.next.is_empty() { break; }
        after = page.next;
    }
    
    let mut drift = Vec::new();
    
    for opening in &opened {
        let account_id = opening.get("account").and_then(Value::as_str).unwrap_or("");
        let start_units = opening.get("units").and_then(Value::as_i64).unwrap_or(0);
        
        let account_entry = match records::get("accounts", account_id) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let account_doc: Value = serde_json::from_str(&account_entry.data).unwrap_or(json!({}));
        let currency = account_doc.get("currency").and_then(Value::as_str).unwrap_or("EUR").to_string();
        let actual_units = account_doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let actual_amount = money::Amount { units: actual_units, currency: currency.clone() };
        
        let mut expected_amount = money::Amount { units: start_units, currency: currency.clone() };
        
        for j in &all_journal_lines {
            let from_acc = j.get("from").and_then(Value::as_str).unwrap_or("");
            let to_acc = j.get("to").and_then(Value::as_str).unwrap_or("");
            let units = j.get("units").and_then(Value::as_i64).unwrap_or(0);
            
            if from_acc == account_id {
                let diff = money::Amount { units, currency: currency.clone() };
                expected_amount = money::subtract(&expected_amount, &diff).unwrap_or(expected_amount);
            }
            if to_acc == account_id {
                let diff = money::Amount { units, currency: currency.clone() };
                expected_amount = money::add(&expected_amount, &diff).unwrap_or(expected_amount);
            }
        }
        
        if expected_amount.units != actual_amount.units {
            drift.push(json!({
                "account": account_id,
                "expected": expected_amount.units,
                "actual": actual_amount.units,
                "delta": actual_amount.units - expected_amount.units
            }));
        }
    }
    
    let balanced = drift.is_empty();
    
    let res = json!({
        "checked": opened.len(),
        "drift": drift,
        "balanced": balanced,
        "journal_lines": journal_lines
    });
    
    if !route.idempotency_key.is_empty() {
        let body_bytes = serde_json::to_vec(&res).unwrap();
        let _ = idem::complete(&route.idempotency_key, 200, &body_bytes);
    }
    
    Reply::json(200, res)
}

fn list_journal(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    
    let limit_str = route.param("limit");
    let limit = limit_str.parse::<u32>().unwrap_or(50).min(500);
    
    let mut lines = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records("journal", limit, &after) {
            Ok(p) => p,
            Err(_) => return Reply::err(503, "store_unavailable"),
        };
        let empty = page.entries.is_empty();
        for e in page.entries {
            if let Ok(mut v) = serde_json::from_str::<Value>(&e.data) {
                if let Some(o) = v.as_object_mut() {
                    o.insert("id".into(), json!(e.id));
                }
                lines.push(v);
                if lines.len() >= limit as usize {
                    break;
                }
            }
        }
        if lines.len() >= limit as usize { break; }
        if empty || page.next.is_empty() { break; }
        after = page.next;
    }
    
    lines.sort_by(|a, b| {
        a.get("at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("at").and_then(Value::as_str).unwrap_or(""))
    });
    
    Reply::json(200, json!({"lines": lines}))
}
