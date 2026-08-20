//! `reply` — the model drafts it, and the outbox owns it.
//!
//! This part must never send. A drafted reply is ENQUEUED and the courier part delivers
//! it later, so a far end that is down when this runs still gets the reply once it comes
//! back up. See CONTRACT.md for the exact order of checks and error shapes.

use serde_json::json;

use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::outbox::dispatch::queue as outbox;
use crate::bindings::quota::meter::meter::{self, QuotaError};
use crate::bindings::records::store::store as records;
use crate::bindings::session::store::store::{self as sessions, SessionError};
use crate::bindings::wasi::http::types::Method;
use crate::{cfg_u64, now_secs, rfc3339, Reply, Route};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(404, "not_found");
    }
    let Some(id) = route.segments.get(2) else {
        return Reply::err(404, "not_found");
    };

    // 1. CSRF. Nothing else runs first: a request that did not come from the page is not
    // a request.
    if route.session.is_empty() || route.csrf.is_empty() {
        return Reply::err(403, "csrf_required");
    }
    if let Err(e) = sessions::verify_csrf(&route.session, &route.csrf) {
        return match e {
            SessionError::CsrfMismatch => Reply::err(403, "csrf_invalid"),
            SessionError::NotFound => Reply::err(403, "session_expired"),
            SessionError::BackendUnavailable(_) => Reply::err(503, "session_unavailable"),
        };
    }

    // Identity + scope.
    let required = Permission { target: "tickets".to_string(), action: "reply".to_string() };
    let principal = match authz::authorize(&route.bearer, &required) {
        Ok(p) => p,
        Err(e) => {
            let (status, code) = match e {
                AuthError::InvalidToken(_) | AuthError::Expired | AuthError::Malformed(_) => {
                    (401, "unauthenticated")
                }
                AuthError::InsufficientScope(_) => (403, "forbidden"),
                _ => (503, "auth_unavailable"),
            };
            return Reply::err(status, code);
        }
    };

    // 2. The ticket must exist and be open.
    let entry = match records::get("tickets", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut ticket: serde_json::Value =
        serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
    if ticket.get("state").and_then(serde_json::Value::as_str) != Some("open") {
        return Reply::err(409, "already_answered");
    }
    let subject = ticket.get("subject").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let body_text = ticket.get("body").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let customer = ticket.get("customer").and_then(serde_json::Value::as_str).unwrap_or("").to_string();

    // 3. Budget: per tenant, not per subject.
    let limit = cfg_u64("reply-budget", 50);
    let period = cfg_u64("reply-period-secs", 86400);
    let balance = match meter::reserve(&principal.tenant, 1, limit, period) {
        Ok(b) => b,
        Err(QuotaError::Exceeded(_)) => {
            // The exceeded payload is units still available, not a duration — always 0
            // for a refused single-unit request. The real wait comes from peek.
            let retry_after = match meter::peek(&principal.tenant, limit, period) {
                Ok(b) => b.resets_at.saturating_sub(now_secs()),
                Err(_) => period,
            };
            return Reply::json(
                429,
                json!({ "error": "budget_exhausted", "retry_after": retry_after }),
            );
        }
        Err(QuotaError::BackendUnavailable(_)) => return Reply::err(503, "quota_unavailable"),
    };

    // 4. The draft.
    let draft = match ai::generate(&subject, &body_text) {
        Ok(text) => text,
        Err(_) => return Reply::err(503, "draft_unavailable"),
    };

    // 5. Enqueue it. Never send it — that is the courier's job, and sending here is the
    // one failure this whole app exists to avoid.
    let payload = json!({
        "ticket": id,
        "target": customer,
        "subject": format!("Re: {subject}"),
        "body": draft,
    });
    let event_id = match outbox::enqueue("support.reply", payload.to_string().as_bytes(), 0) {
        Ok(id) => id,
        Err(_) => return Reply::err(503, "outbox_unavailable"),
    };

    let drafted_at = rfc3339(now_secs());
    ticket["state"] = json!("answered");
    ticket["reply"] = json!({ "text": draft, "event": event_id, "drafted_at": drafted_at });
    if records::update("tickets", id, &ticket.to_string(), entry.revision).is_err() {
        // The reply is enqueued and will be delivered regardless; the ticket's own record
        // just failed to reflect it. Surfacing this as a 500 tells the caller something is
        // wrong without pretending the enqueue can be undone.
        return Reply::err(500, "ticket_update_failed");
    }

    Reply::json(202, json!({ "event": event_id, "remaining": balance.remaining }))
}