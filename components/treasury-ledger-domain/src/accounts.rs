use crate::{now_secs, Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "accounts"]) => create_account(route, body),
        (Method::Post, ["api", "accounts", id, "credit"]) => credit_account(route, body, id),
        (Method::Get, ["api", "accounts", id]) => get_account(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "accounts".to_string(), action: action.to_string() };
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

fn create_account(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let name = req.get("name").and_then(Value::as_str).unwrap_or("");
    let currency = req.get("currency").and_then(Value::as_str).unwrap_or("");
    
    if name.is_empty() || currency.is_empty() {
        return Reply::err(400, "invalid_account");
    }
    
    let start_str = req.get("start").and_then(Value::as_str);
    let amount = if let Some(s) = start_str {
        match money::parse(s, currency) {
            Ok(a) => a,
            Err(_) => return Reply::err(400, "bad_money"),
        }
    } else {
        let zero_amount = money::Amount { units: 0, currency: currency.to_string() };
        let formatted_zero = match money::format(&zero_amount) {
            Ok(f) => f,
            Err(_) => return Reply::err(400, "bad_money"),
        };
        match money::parse(&formatted_zero, currency) {
            Ok(a) => a,
            Err(_) => return Reply::err(400, "bad_money"),
        }
    };
    
    let doc = json!({
        "name": name,
        "currency": currency,
        "units": amount.units,
        "opened_at": guestfmt::rfc3339(now_secs())
    });
    
    match records::create("accounts", &doc.to_string(), &["name".to_string()]) {
        Ok(e) => Reply::json(201, json!({"id": e.id})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn credit_account(route: &Route, body: &str, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let amount_str = req.get("amount").and_then(Value::as_str).unwrap_or("");
    
    let mut attempts = 0;
    loop {
        if attempts >= 20 {
            return Reply::err(503, "contended");
        }
        attempts += 1;
        
        let entry = match records::get("accounts", id) {
            Ok(e) => e,
            Err(_) => return Reply::err(404, "not_found"),
        };
        
        let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let currency = doc.get("currency").and_then(Value::as_str).unwrap_or("");
        
        let amount = match money::parse(amount_str, currency) {
            Ok(a) => a,
            Err(_) => return Reply::err(400, "invalid_amount"),
        };
        
        if amount.units <= 0 {
            return Reply::err(400, "invalid_amount");
        }
        
        let units = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let cur_amount = money::Amount { units, currency: currency.to_string() };
        
        let new_amount = match money::add(&cur_amount, &amount) {
            Ok(a) => a,
            Err(_) => return Reply::err(500, "money_error"),
        };
        
        doc["units"] = json!(new_amount.units);
        
        match records::update("accounts", id, &doc.to_string(), entry.revision) {
            Ok(_) => return Reply::json(200, json!({"units": new_amount.units})),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Reply::err(503, "store_error"),
        }
    }
}

fn get_account(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    match records::get("accounts", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
                m.insert("revision".to_string(), json!(e.revision));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}
