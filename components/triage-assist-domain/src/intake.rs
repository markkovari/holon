use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::pii::redact::redactor as pii;
use crate::bindings::pii::redact::redactor::Options;
use crate::bindings::ratelimit::guard::limiter as rl;
use crate::bindings::ratelimit::guard::limiter::LimitError;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{ledger, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reports"]) => create_report(route, body),
        (Method::Get, ["api", "reports", id]) => get_report(route, id),
        (Method::Get, ["api", "reports"]) => list_reports(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str, event: &str) -> Result<String, Reply> {
    let perm = Permission { target: "reports".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            let subj = authz::introspect(&route.bearer).map(|p| p.subject).unwrap_or_default();
            if event == "reports.create" {
                ledger::note(&route.trace, event, "denied", &subj, "");
            }
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                    Reply::err(503, "auth_unavailable")
                }
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn create_report(route: &Route, body: &str) -> Reply {
    let subject = match authorize_perm(route, "write", "reports.create") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let title = req.get("title").and_then(Value::as_str).unwrap_or("");
    let report_body = req.get("body").and_then(Value::as_str).unwrap_or("");
    let component = req.get("component").and_then(Value::as_str).unwrap_or("");

    if title.is_empty() || report_body.is_empty() || component.is_empty() {
        return Reply::err(400, "invalid_report");
    }

    match rl::check(&subject) {
        Ok(_) => {}
        Err(LimitError::Locked(secs)) => {
            ledger::note(&route.trace, "reports.create", "throttled", &subject, "");
            return Reply::json(429, json!({"error": "rate_limited", "retry_after": secs}));
        }
        Err(LimitError::BackendUnavailable(_)) => return Reply::err(503, "rate_limit_unavailable"),
    }

    if rl::record_failure(&subject).is_err() {
        return Reply::err(503, "rate_limit_unavailable");
    }

    let masked_body = pii::redact(report_body, &Options { kinds: vec![] });

    let data = json!({
        "title": title,
        "body": masked_body,
        "component": component,
        "state": "open",
        "reporter": subject,
        "reported_at": crate::rfc3339(crate::now_secs())
    })
    .to_string();

    match records::create("reports", &data, &["component".to_string(), "state".to_string()]) {
        Ok(entry) => {
            ledger::note(&route.trace, "reports.create", "ok", &subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn get_report(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read", "") {
        return r;
    }
    match records::get("reports", id) {
        Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn list_reports(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read", "") {
        return r;
    }

    let comp = route.param("component");
    let state = route.param("state");

    let entries = if !comp.is_empty() {
        records::find_by("reports", "component", &json!(comp).to_string()).unwrap_or_default()
    } else if !state.is_empty() {
        records::find_by("reports", "state", &json!(state).to_string()).unwrap_or_default()
    } else {
        records::list_records("reports", 100, "").map(|p| p.entries).unwrap_or_default()
    };

    // If both filters were provided, we must do in-memory filtering for the second
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            let v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            let c = v.get("component").and_then(Value::as_str).unwrap_or("");
            let s = v.get("state").and_then(Value::as_str).unwrap_or("");
            (comp.is_empty() || c == comp) && (state.is_empty() || s == state)
        })
        .collect();

    let items: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            v
        })
        .collect();

    Reply::json(200, json!({"reports": items}))
}
