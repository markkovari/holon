use crate::{cfg, now_secs, Reply, Route};
use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::event::bus::bus;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "items", id, "review"]) => review_item(route, id),
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

fn review_item(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "moderate") {
        return r;
    }
    
    let entry = match records::get("items", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let state = doc.get("state").and_then(Value::as_str).unwrap_or("");
    if state != "pending" {
        return Reply::json(409, json!({
            "error": "already_decided",
            "final": state
        }));
    }
    
    let text = doc.get("text").and_then(Value::as_str).unwrap_or("");
    let author = doc.get("author").and_then(Value::as_str).unwrap_or("");
    
    let labels = vec!["allow".to_string(), "flag".to_string(), "block".to_string()];
    let model_res = match ai::classify(text, &labels) {
        Ok(l) => {
            if !labels.contains(&l.label) {
                return Reply::err(502, "unexpected_label");
            }
            l
        }
        Err(_) => return Reply::err(503, "model_unavailable"),
    };
    
    let has_link = if text.contains("://") { "true" } else { "false" };
    
    let p_attrs = vec![
        policy::Attr { key: "subject".to_string(), value: author.to_string() },
    ];
    let t_attrs = vec![
        policy::Attr { key: "model_label".to_string(), value: model_res.label.clone() },
        policy::Attr { key: "has_link".to_string(), value: has_link.to_string() },
        policy::Attr { key: "author".to_string(), value: author.to_string() },
    ];
    
    let dec = match policy::can(&cfg("policy-domain", "moderation"), "publish", &p_attrs, &t_attrs) {
        Ok(d) => d,
        Err(_) => return Reply::err(503, "policy_unavailable"),
    };
    
    let final_decision = if !dec.rule_id.is_empty() {
        if dec.allowed { "allowed" } else { "blocked" }
    } else {
        match model_res.label.as_str() {
            "allow" => "allowed",
            "flag" => "flagged",
            "block" => "blocked",
            _ => "blocked"
        }
    };
    
    let decision_obj = json!({
        "final": final_decision,
        "model_said": model_res.label,
        "model_confidence": model_res.confidence,
        "policy_rule": dec.rule_id,
        "policy_reason": dec.reason,
        "decided_at": guestfmt::rfc3339(now_secs())
    });
    
    doc["state"] = json!(final_decision);
    doc["decision"] = decision_obj.clone();
    
    if records::update("items", id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }
    
    let event_payload = json!({
        "item": id,
        "final": final_decision
    });
    
    if bus::publish("moderation.decided", &serde_json::to_vec(&event_payload).unwrap()).is_err() {
        return Reply::err(503, "bus_unavailable");
    }
    
    Reply::json(200, decision_obj)
}
