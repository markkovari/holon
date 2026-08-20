//! `ledger` — what happened, durably and queryably.
//!
//! NOT IMPLEMENTED. This is your file, and `note` is the protocol: the router and
//! both other parts call it, so its signature is not yours to change — only its
//! body. `audit:log/recorder` is where an event goes and `audit:log/query` is how it
//! comes back.
//!
//! `note` returning nothing is deliberate. An audit backend that is down is a `note`
//! that did nothing, never a 500 on somebody else's report.

use crate::bindings::audit::log::query as audit_query;
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
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

fn event_json(e: &Event) -> serde_json::Value {
    json!({
        "id": e.id,
        "trace_id": e.trace_id,
        "span_id": e.span_id,
        "timestamp": e.timestamp,
        "event": e.event,
        "outcome": e.outcome,
        "tenant": e.tenant,
        "subject": e.subject,
        "detail": e.detail,
    })
}

fn auth_reply(err: AuthError) -> Reply {
    match err {
        AuthError::InvalidToken(_) | AuthError::Expired | AuthError::Malformed(_) => {
            Reply::err(401, "unauthenticated")
        }
        AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
        _ => Reply::err(503, "auth_unavailable"),
    }
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Get) {
        return Reply::err(404, "not_found");
    }
    let required = Permission { target: "reports".to_string(), action: "read".to_string() };
    if let Err(e) = authorizer::authorize(&route.bearer, &required) {
        return auth_reply(e);
    }

    let trace = route.param("trace");
    let events = if !trace.is_empty() {
        audit_query::by_trace(&trace)
    } else {
        let limit: u32 = route.param("limit").parse().unwrap_or(20).clamp(1, 100);
        audit_query::recent(limit)
    };

    match events {
        Ok(list) => Reply::json(200, json!({ "events": list.iter().map(event_json).collect::<Vec<_>>() })),
        Err(_) => Reply::err(503, "audit_unavailable"),
    }
}
