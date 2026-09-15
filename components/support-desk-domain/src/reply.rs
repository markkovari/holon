use crate::{cfg_u64, now_secs, Reply, Route};
use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::outbox::dispatch::queue as outbox;
use crate::bindings::quota::meter::meter;
use crate::bindings::records::store::store as records;
use crate::bindings::session::store::store as sessions;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "tickets", id, "reply"]) => write_reply(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<Principal, Reply> {
    let perm = Permission { target: "tickets".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p),
        Err(err) => {
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => Reply::err(503, "auth_unavailable"),
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn write_reply(route: &Route, id: &str) -> Reply {
    if route.session.is_empty() || route.csrf.is_empty() {
        return Reply::err(403, "csrf_required");
    }
    match sessions::verify_csrf(&route.session, &route.csrf) {
        Ok(_) => {}
        Err(sessions::SessionError::CsrfMismatch) => return Reply::err(403, "csrf_invalid"),
        Err(sessions::SessionError::NotFound) => return Reply::err(403, "session_expired"),
        Err(sessions::SessionError::BackendUnavailable(_)) => return Reply::err(503, "session_unavailable"),
    }

    let principal = match authorize_perm(route, "reply") {
        Ok(p) => p,
        Err(r) => return r,
    };
    
    let entry = match records::get("tickets", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    if doc.get("state").and_then(Value::as_str).unwrap_or("") != "open" {
        return Reply::err(409, "already_answered");
    }

    let budget = cfg_u64("reply-budget", 50);
    let period = cfg_u64("reply-period-secs", 86400);
    let balance = match meter::reserve(&principal.tenant, 1, budget, period) {
        Ok(b) => b,
        Err(meter::QuotaError::Exceeded(_)) => {
            let resets_at = match meter::peek(&principal.tenant, budget, period) {
                Ok(b) => b.resets_at,
                Err(_) => now_secs(),
            };
            let retry_after = resets_at.saturating_sub(now_secs());
            return Reply::json(429, json!({"error": "budget_exhausted", "retry_after": retry_after}));
        }
        Err(_) => return Reply::err(503, "budget_unavailable"),
    };

    let subject = doc.get("subject").and_then(Value::as_str).unwrap_or("");
    let body = doc.get("body").and_then(Value::as_str).unwrap_or("");
    let draft = match ai::generate(subject, body) {
        Ok(d) => d,
        Err(_) => return Reply::err(503, "draft_unavailable"),
    };

    let customer = doc.get("customer").and_then(Value::as_str).unwrap_or("");
    let payload = json!({
        "ticket": id,
        "target": customer,
        "subject": format!("Re: {}", subject),
        "body": draft
    });

    let event_id = match outbox::enqueue("support.reply", payload.to_string().as_bytes(), 0) {
        Ok(id) => id,
        Err(_) => return Reply::err(503, "outbox_unavailable"),
    };

    doc["state"] = json!("answered");
    doc["reply"] = json!({
        "text": draft,
        "event": event_id,
        "drafted_at": guestfmt::rfc3339(now_secs())
    });

    if records::update("tickets", id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    Reply::json(202, json!({
        "event": event_id,
        "remaining": balance.remaining
    }))
}
