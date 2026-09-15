use crate::{Reply, Route};
use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "invoices", id, "lines", "suggest"]) => suggest_lines(route, body, id),
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

fn suggest_lines(route: &Route, body: &str, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let prose = req.get("prose").and_then(Value::as_str).unwrap_or("");
    let total_str = req.get("total").and_then(Value::as_str).unwrap_or("");
    let shares = req.get("shares").and_then(Value::as_u64).unwrap_or(0);
    
    if prose.is_empty() || total_str.is_empty() || shares < 2 || shares > 12 {
        return Reply::err(400, "invalid_suggestion");
    }
    
    let entry = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    if doc.get("state").and_then(Value::as_str).unwrap_or("") != "draft" {
        return Reply::err(409, "already_posted");
    }
    
    let currency = doc.get("currency").and_then(Value::as_str).unwrap_or("");
    
    let total_amount = match money::parse(total_str, currency) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_money"),
    };
    
    let mut fields = Vec::new();
    for i in 1..=shares {
        fields.push(format!("line_description_{}", i));
    }
    
    let extracted = match ai::extract(prose, &fields) {
        Ok(e) => e,
        Err(_) => return Reply::err(503, "suggest_unavailable"),
    };
    
    let mut descriptions = Vec::new();
    for i in 0..shares as usize {
        if i < extracted.len() {
            descriptions.push(extracted[i].1.clone());
        } else {
            descriptions.push(format!("Line {}", i + 1));
        }
    }
    
    let allocated = match money::allocate(&total_amount, shares as u32) {
        Ok(a) => a,
        Err(_) => return Reply::err(500, "money_error"),
    };
    
    let mut lines = Vec::new();
    let mut running_sum = money::Amount { units: 0, currency: currency.to_string() };
    for (i, amt) in allocated.iter().enumerate() {
        lines.push(json!({
            "memo": descriptions[i],
            "units": amt.units
        }));
        if let Ok(new_sum) = money::add(&running_sum, amt) {
            running_sum = new_sum;
        } else {
            return Reply::err(500, "money_error");
        }
    }
    
    if running_sum.units != total_amount.units {
        return Reply::err(500, "allocation_failed");
    }
    
    doc["lines"] = json!(lines);
    doc["total_units"] = json!(total_amount.units);
    
    if records::update("invoices", id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }
    
    let formatted_total = money::format(&total_amount).unwrap_or(total_str.to_string());
    
    Reply::json(200, json!({
        "lines": lines,
        "total_units": total_amount.units,
        "total": formatted_total
    }))
}
