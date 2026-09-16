use crate::{ledger, Reply, Route};
use crate::bindings::ai::inference::inference as ai;
use crate::bindings::ai::inference::inference::Length;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reports", id, "assist"]) => create_assist(route, id),
        (Method::Get, ["api", "reports", id, "assist"]) => get_assist(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "reports".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
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

fn create_assist(route: &Route, id: &str) -> Reply {
    let subject = match authorize_perm(route, "write") {
        Ok(s) => s,
        Err(r) => return r,
    };

    let entry = match records::get("reports", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut report: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    
    if report.get("assist").is_some() {
        let severity = report["assist"].get("severity").and_then(Value::as_str).unwrap_or("");
        return Reply::json(409, json!({
            "error": "already_assisted",
            "severity": severity
        }));
    }

    let title = report.get("title").and_then(Value::as_str).unwrap_or("");
    let body = report.get("body").and_then(Value::as_str).unwrap_or("");
    let text = format!("{}\n{}", title, body);

    let labels = vec!["critical".to_string(), "major".to_string(), "minor".to_string()];
    let classify_result = ai::classify(&text, &labels);
    let summarize_result = ai::summarize(&text, Length::Brief, "what is broken and where");

    match (classify_result, summarize_result) {
        (Ok(class), Ok(summary)) => {
            if !labels.contains(&class.label) {
                ledger::note(&route.trace, "reports.assist", "error", &subject, "unexpected_severity");
                return Reply::err(502, "unexpected_severity");
            }
            let assist_data = json!({
                "severity": class.label,
                "confidence": class.confidence,
                "summary": summary,
                "assisted_at": crate::rfc3339(crate::now_secs())
            });
            report["assist"] = assist_data.clone();
            if records::update("reports", id, &report.to_string(), entry.revision).is_err() {
                return Reply::err(500, "store_error");
            }
            ledger::note(&route.trace, "reports.assist", "ok", &subject, id);
            
            Reply::json(200, json!({
                "severity": class.label,
                "confidence": class.confidence,
                "summary": summary
            }))
        }
        _ => {
            ledger::note(&route.trace, "reports.assist", "error", &subject, "assist_unavailable");
            Reply::err(503, "assist_unavailable")
        }
    }
}

fn get_assist(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    let entry = match records::get("reports", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let report: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    if let Some(assist) = report.get("assist") {
        Reply::json(200, assist.clone())
    } else {
        Reply::err(404, "not_assisted")
    }
}
