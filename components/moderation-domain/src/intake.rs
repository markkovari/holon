//! `intake` — what gets into the queue at all.
//!
//! `POST /api/items` and `GET /api/items/{id}`. See `CONTRACT.md` Part 1.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::ratelimit::guard::limiter as rl;
use crate::bindings::ratelimit::guard::limiter::LimitError;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

/// Map an `AuthError` to the contract's status/body, per the identity table.
fn auth_reply(err: AuthError) -> Reply {
    match err {
        AuthError::InvalidToken(_) | AuthError::Expired | AuthError::Malformed(_) => {
            Reply::err(401, "unauthenticated")
        }
        AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
        AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
            Reply::err(503, "auth_unavailable")
        }
        // Not reachable via `authorize` on this path, but covered for completeness.
        AuthError::UnknownTenant => Reply::err(403, "forbidden"),
        AuthError::InvalidCredentials | AuthError::AlreadyExists => {
            Reply::err(401, "unauthenticated")
        }
        AuthError::RateLimited(secs) => {
            Reply::json(429, json!({ "error": "rate_limited", "retry_after": secs }))
        }
    }
}

fn create_item(route: &Route, body: &str) -> Reply {
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthenticated");
    }
    let required = Permission { target: "items".into(), action: "write".into() };
    let principal = match authz::authorize(&route.bearer, &required) {
        Ok(p) => p,
        Err(e) => return auth_reply(e),
    };

    let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let text = req.get("text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return Reply::err(400, "invalid_item");
    }

    // The throttle: `check` only asks, `record_failure` is what makes it count.
    // Keyed on `principal.subject`, not anything the caller supplied.
    let key = &principal.subject;
    match rl::check(key) {
        Ok(_remaining) => {}
        Err(LimitError::Locked(secs)) => {
            return Reply::json(429, json!({ "error": "rate_limited", "retry_after": secs }))
        }
        Err(LimitError::BackendUnavailable(_)) => return Reply::err(503, "rate_limit_unavailable"),
    }

    let entry = match records::create(
        "items",
        &json!({
            "text": text,
            "author": principal.subject,
            "state": "pending",
            "submitted_at": rfc3339(now_secs()),
        })
        .to_string(),
        &["state".to_string(), "author".to_string()],
    ) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_unavailable"),
    };

    // Count this accepted submission against the window.
    match rl::record_failure(key) {
        Ok(()) => {}
        Err(LimitError::BackendUnavailable(_)) => return Reply::err(503, "rate_limit_unavailable"),
        Err(LimitError::Locked(_)) => {} // already past `check`; item is already stored
    }

    Reply::json(201, json!({ "id": entry.id }))
}

fn get_item(route: &Route, id: &str) -> Reply {
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthenticated");
    }
    let required = Permission { target: "items".into(), action: "read".into() };
    if let Err(e) = authz::authorize(&route.bearer, &required) {
        return auth_reply(e);
    }

    match records::get("items", id) {
        Ok(entry) => {
            let mut item: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
            if let Some(obj) = item.as_object_mut() {
                obj.insert("id".to_string(), json!(entry.id));
            }
            Reply::json(200, item)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "items"]) => create_item(route, body),
        (Method::Get, ["api", "items", id]) => get_item(route, id),
        _ => Reply::err(404, "not_found"),
    }
}
