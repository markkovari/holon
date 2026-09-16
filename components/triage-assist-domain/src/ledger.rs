use crate::{Reply, Route};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::query as audit_query;
use crate::bindings::audit::log::types::Event;
use crate::bindings::wasi::http::types::Method;
use serde_json::json;

pub fn note(trace: &str, event: &str, outcome: &str, subject: &str, detail: &str) {
    let _ = audit::record_event(&Event {
        id: String::new(),
        trace_id: trace.to_string(),
        span_id: String::new(),
        timestamp: 0,
        event: event.to_string(),
        outcome: outcome.to_string(),
        tenant: "triage-assist".to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
    });
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Get) {
        return Reply::err(404, "not_found");
    }

    use crate::bindings::auth::identity::authorizer as authz;
    use crate::bindings::auth::identity::types::{Permission, AuthError};
    let perm = Permission { target: "reports".to_string(), action: "read".to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(_) => {}
        Err(AuthError::InsufficientScope(_)) => return Reply::err(403, "forbidden"),
        Err(AuthError::BackendUnavailable(_) | AuthError::Internal(_)) => return Reply::err(503, "auth_unavailable"),
        Err(_) => return Reply::err(401, "unauthenticated"),
    }

    let trace = route.param("trace");
    let events = if !trace.is_empty() {
        audit_query::by_trace(&trace).unwrap_or_default()
    } else {
        let limit = route.param("limit").parse::<u32>().unwrap_or(20).min(100);
        audit_query::recent(limit).unwrap_or_default()
    };

    let items: Vec<serde_json::Value> = events.into_iter().map(|e| {
        json!({
            "id": e.id,
            "trace_id": e.trace_id,
            "span_id": e.span_id,
            "timestamp": e.timestamp,
            "event": e.event,
            "outcome": e.outcome,
            "tenant": e.tenant,
            "subject": e.subject,
            "detail": e.detail
        })
    }).collect();

    Reply::json(200, json!({ "events": items }))
}
