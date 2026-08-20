//! `verdict` — the model's opinion, and what the policy does to it.
//!
//! Precedence is the whole of this part: a rule the policy engine matched decides
//! (`rule_id` non-empty); when nothing matched (`rule_id` empty, default deny) the
//! model's label decides instead. Both are recorded, because a decision that reports
//! only its outcome cannot be audited.

use crate::bindings::ai::inference::inference as ai;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::event::bus::bus;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg, now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(404, "not_found");
    }
    let Some(id) = route.segments.get(2) else {
        return Reply::err(404, "not_found");
    };

    if route.bearer.is_empty() {
        return Reply::err(401, "unauthenticated");
    }
    let required = auth_types::Permission { target: "items".into(), action: "moderate".into() };
    if let Err(e) = authz::authorize(&route.bearer, &required) {
        return match e {
            auth_types::AuthError::InvalidToken(_)
            | auth_types::AuthError::Expired
            | auth_types::AuthError::Malformed(_) => Reply::err(401, "unauthenticated"),
            auth_types::AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
            auth_types::AuthError::BackendUnavailable(_) | auth_types::AuthError::Internal(_) => {
                Reply::err(503, "auth_unavailable")
            }
            _ => Reply::err(401, "unauthenticated"),
        };
    }

    // The item must exist and still be pending — no model call otherwise.
    let entry = match records::get("items", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut item: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let state = item.get("state").and_then(Value::as_str).unwrap_or("").to_string();
    if state != "pending" {
        return Reply::json(409, json!({ "error": "already_decided", "final": state }));
    }
    let text = item.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let author = item.get("author").and_then(Value::as_str).unwrap_or("").to_string();

    // 1. The model's opinion.
    let labels = vec!["allow".to_string(), "flag".to_string(), "block".to_string()];
    let label_score = match ai::classify(&text, &labels) {
        Ok(ls) => ls,
        Err(_) => return Reply::err(503, "model_unavailable"),
    };
    if !labels.contains(&label_score.label) {
        return Reply::err(502, "unexpected_label");
    }

    // 2. The policy, which decides. `has_link`/`model_label`/`author` are written
    // under exactly those keys so a rule can reference `resource.<key>`.
    let has_link = text.contains("://");
    let domain = cfg("policy-domain", "moderation");
    let principal_attrs = vec![policy::Attr { key: "subject".into(), value: author.clone() }];
    let target_attrs = vec![
        policy::Attr { key: "model_label".into(), value: label_score.label.clone() },
        policy::Attr { key: "has_link".into(), value: has_link.to_string() },
        policy::Attr { key: "author".into(), value: author.clone() },
    ];
    let decision = match policy::can(&domain, "publish", &principal_attrs, &target_attrs) {
        Ok(d) => d,
        Err(_) => return Reply::err(503, "policy_unavailable"),
    };

    // THE trap: an empty rule_id is "no rule matched", not "denied" — only a
    // non-empty rule_id means the policy actually decided anything.
    let final_state = if !decision.rule_id.is_empty() {
        if decision.allowed { "allowed" } else { "blocked" }
    } else {
        match label_score.label.as_str() {
            "allow" => "allowed",
            "flag" => "flagged",
            _ => "blocked",
        }
    };

    let decision_json = json!({
        "final": final_state,
        "model_said": label_score.label,
        "model_confidence": label_score.confidence,
        "policy_rule": decision.rule_id,
        "policy_reason": decision.reason,
        "decided_at": rfc3339(now_secs()),
    });
    item["decision"] = decision_json.clone();
    item["state"] = json!(final_state);

    if records::update("items", id, &item.to_string(), 0).is_err() {
        return Reply::err(500, "internal");
    }

    // 3. Publish it. The decision already stands; a publish failure is reported,
    // not swallowed, but it does not undo the write.
    let payload = json!({ "item": id, "final": final_state }).to_string();
    if bus::publish("moderation.decided", payload.as_bytes()).is_err() {
        return Reply::err(503, "bus_unavailable");
    }

    Reply::json(200, decision_json)
}