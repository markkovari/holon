use crate::{now_secs, Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::ratelimit::guard::limiter as rl;
use crate::bindings::ratelimit::guard::limiter::LimitError;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "items"]) => create_item(route, body),
        (Method::Get, ["api", "items", id]) => get_item(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "items".to_string(), action: action.to_string() };
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

fn create_item(route: &Route, body: &str) -> Reply {
    let subject = match authorize_perm(route, "write") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let text = req.get("text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return Reply::err(400, "invalid_item");
    }

    match rl::check(&subject) {
        Ok(_) => {}
        Err(LimitError::Locked(secs)) => {
            return Reply::json(429, json!({"error": "rate_limited", "retry_after": secs}));
        }
        Err(LimitError::BackendUnavailable(_)) => return Reply::err(503, "rate_limit_unavailable"),
    }

    if rl::record_failure(&subject).is_err() {
        return Reply::err(503, "rate_limit_unavailable");
    }

    let doc = json!({
        "text": text,
        "author": subject,
        "state": "pending",
        "submitted_at": guestfmt::rfc3339(now_secs())
    }).to_string();

    match records::create("items", &doc, &["state".to_string(), "author".to_string()]) {
        Ok(e) => Reply::json(201, json!({"id": e.id})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn get_item(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    match records::get("items", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}
