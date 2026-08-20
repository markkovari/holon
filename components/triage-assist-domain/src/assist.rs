//! `assist` — what the model thinks is wrong, and how badly.
//!
//! Reads the report FROM THE STORE (never the request body, which is empty for
//! this route), asks the model the two questions the contract names, and writes
//! the answer onto the report. A provider that is down leaves the report
//! exactly as it was: the store write only happens after both model calls
//! succeed.

use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as auth;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{ledger, now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

const SEVERITIES: [&str; 3] = ["critical", "major", "minor"];

/// Authorize the route's bearer for `action` on `reports`, per CONTRACT.md's
/// failure table. `authorize` (not a hand-rolled JWT parse) does verification
/// and the scope check in one call.
fn authorize(route: &Route, action: &str) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = auth_types::Permission { target: "reports".into(), action: action.into() };
    match auth::authorize(&route.bearer, &required) {
        Ok(p) => Ok(p),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_)) | Err(auth_types::AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    // route.segments == ["api", "reports", "<id>", "assist"]
    let id = match route.segments.get(2) {
        Some(id) => id.clone(),
        None => return Reply::err(404, "not_found"),
    };
    match method {
        Method::Post => handle_post(route, &id),
        Method::Get => handle_get(route, &id),
        _ => Reply::err(404, "not_found"),
    }
}

fn handle_get(route: &Route, id: &str) -> Reply {
    if let Err(reply) = authorize(route, "read") {
        return reply;
    }
    let entry = match records::get("reports", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    match doc.get("assist") {
        Some(assist) if !assist.is_null() => Reply::json(200, assist.clone()),
        _ => Reply::err(404, "not_assisted"),
    }
}

fn handle_post(route: &Route, id: &str) -> Reply {
    let principal = match authorize(route, "write") {
        Ok(p) => p,
        Err(reply) => return reply,
    };

    let entry = match records::get("reports", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));

    // Already assisted: a 409 naming the stored severity, not a second model call.
    if let Some(assist) = doc.get("assist") {
        if !assist.is_null() {
            let severity = assist.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
            return Reply::json(409, json!({ "error": "already_assisted", "severity": severity }));
        }
    }

    // The stored (masked) title + body — never the request, which is empty here.
    let title = doc.get("title").and_then(Value::as_str).unwrap_or("");
    let body = doc.get("body").and_then(Value::as_str).unwrap_or("");
    let text = format!("{}\n{}", title, body);

    let labels: Vec<String> = SEVERITIES.iter().map(|s| s.to_string()).collect();
    let score = match ai::classify(&text, &labels) {
        Ok(s) => s,
        Err(_) => {
            ledger::note(&route.trace, "reports.assist", "error", &principal.subject, "classify unavailable");
            return Reply::err(503, "assist_unavailable");
        }
    };
    if !SEVERITIES.contains(&score.label.as_str()) {
        return Reply::err(502, "unexpected_severity");
    }

    let summary = match ai::summarize(&text, ai::Length::Brief, "what is broken and where") {
        Ok(s) => s,
        Err(_) => {
            ledger::note(&route.trace, "reports.assist", "error", &principal.subject, "summarize unavailable");
            return Reply::err(503, "assist_unavailable");
        }
    };

    // Only now, with both model calls in hand, do we touch the store: a 503 path
    // must leave the report exactly as it was.
    let assist = json!({
        "severity": score.label,
        // Stored and answered exactly as returned — 0..=1000 milli-units, not a percentage.
        "confidence": score.confidence,
        "summary": summary,
        "assisted_at": rfc3339(now_secs()),
    });
    doc["assist"] = assist.clone();

    if records::update("reports", id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(503, "assist_unavailable");
    }

    // subject is principal.subject — what authorize returned — never the bearer token.
    ledger::note(&route.trace, "reports.assist", "ok", &principal.subject, &format!("severity={}", score.label));

    Reply::json(200, assist)
}