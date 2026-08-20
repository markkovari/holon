//! `intake` — taking a defect report in, authenticated, throttled, and masked.
//!
//! Three refusals (auth, scope, rate-limit) answer differently — see CONTRACT.md.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::pii::redact::redactor as pii;
use crate::bindings::ratelimit::guard::limiter as rl;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{ledger, now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let segs: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, segs.as_slice()) {
        (Method::Post, ["api", "reports"]) => create_report(route, body),
        (Method::Get, ["api", "reports"]) => list_reports(route),
        (Method::Get, ["api", "reports", id]) => get_report(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

/// Verify the bearer and the required `reports:<action>` permission. An absent
/// bearer is refused before ever calling `authorize` — an empty token is not this
/// capability's problem to classify.
fn authorize(route: &Route, action: &str) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = auth_types::Permission {
        target: "reports".into(),
        action: action.into(),
    };
    authz::authorize(&route.bearer, &required).map_err(auth_err_reply)
}

/// The three refusals, kept three answers: unauthenticated, forbidden, unavailable.
fn auth_err_reply(e: auth_types::AuthError) -> Reply {
    use auth_types::AuthError::*;
    match e {
        InvalidToken(_) | Expired | Malformed(_) => Reply::err(401, "unauthenticated"),
        InsufficientScope(_) => Reply::err(403, "forbidden"),
        BackendUnavailable(_) | Internal(_) => Reply::err(503, "auth_unavailable"),
        _ => Reply::err(401, "unauthenticated"),
    }
}

fn create_report(route: &Route, body: &str) -> Reply {
    let principal = match authorize(route, "write") {
        Ok(p) => p,
        Err(reply) => {
            ledger::note(&route.trace, "reports.create", "denied", "", "authorization failed");
            return reply;
        }
    };

    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "invalid_report"),
    };
    let title = req.get("title").and_then(Value::as_str).unwrap_or("").trim();
    let body_text = req.get("body").and_then(Value::as_str).unwrap_or("").trim();
    let component = req.get("component").and_then(Value::as_str).unwrap_or("").trim();
    if title.is_empty() || body_text.is_empty() || component.is_empty() {
        return Reply::err(400, "invalid_report");
    }

    // Check first: a locked key is refused before anything is stored.
    match rl::check(&principal.subject) {
        Ok(_) => {}
        Err(rl::LimitError::Locked(secs)) => {
            ledger::note(&route.trace, "reports.create", "throttled", &principal.subject, "rate limited");
            return Reply::json(429, json!({ "error": "rate_limited", "retry_after": secs }));
        }
        Err(rl::LimitError::BackendUnavailable(_)) => {
            return Reply::err(503, "rate_limit_unavailable");
        }
    }

    // Masked before it ever reaches the store.
    let masked_body = pii::redact(body_text, &pii::Options { kinds: vec![] });

    let doc = json!({
        "title": title,
        "body": masked_body,
        "component": component,
        "state": "open",
        "reporter": principal.subject,
        "reported_at": rfc3339(now_secs()),
    });

    let entry = match records::create(
        "reports",
        &doc.to_string(),
        &["component".to_string(), "state".to_string()],
    ) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_failed"),
    };

    // Record the accepted attempt — this is the call the limiter never limits
    // without.
    match rl::record_failure(&principal.subject) {
        Ok(_) => {}
        Err(rl::LimitError::BackendUnavailable(_)) => {
            return Reply::err(503, "rate_limit_unavailable");
        }
        Err(rl::LimitError::Locked(_)) => {}
    }

    ledger::note(&route.trace, "reports.create", "ok", &principal.subject, &format!("created {}", entry.id));
    Reply::json(201, json!({ "id": entry.id }))
}

fn get_report(route: &Route, id: &str) -> Reply {
    if let Err(reply) = authorize(route, "read") {
        return reply;
    }
    match records::get("reports", id) {
        Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn list_reports(route: &Route) -> Reply {
    if let Err(reply) = authorize(route, "read") {
        return reply;
    }
    let component = route.param("component");
    let state = route.param("state");

    let entries = if component.is_empty() && state.is_empty() {
        match records::list_records("reports", 0, "") {
            Ok(p) => p.entries,
            Err(_) => return Reply::err(500, "list_failed"),
        }
    } else if !component.is_empty() && state.is_empty() {
        match records::find_by("reports", "component", &json!(component).to_string()) {
            Ok(v) => v,
            Err(_) => return Reply::err(500, "list_failed"),
        }
    } else if component.is_empty() && !state.is_empty() {
        match records::find_by("reports", "state", &json!(state).to_string()) {
            Ok(v) => v,
            Err(_) => return Reply::err(500, "list_failed"),
        }
    } else {
        let by_component = match records::find_by("reports", "component", &json!(component).to_string()) {
            Ok(v) => v,
            Err(_) => return Reply::err(500, "list_failed"),
        };
        by_component
            .into_iter()
            .filter(|e| {
                serde_json::from_str::<Value>(&e.data)
                    .ok()
                    .and_then(|d| d.get("state").and_then(Value::as_str).map(str::to_string))
                    .as_deref()
                    == Some(state.as_str())
            })
            .collect()
    };

    let reports: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let mut doc: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Some(obj) = doc.as_object_mut() {
                obj.insert("id".into(), json!(e.id));
            }
            doc
        })
        .collect();

    Reply::json(200, json!({ "reports": reports }))
}