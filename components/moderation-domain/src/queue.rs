use crate::{cfg, Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::event::bus::bus;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "rules"]) => set_rules(route, body),
        (Method::Get, ["api", "rules"]) => get_rules(route),
        (Method::Get, ["api", "queue"]) => list_queue(route),
        (Method::Get, ["api", "events"]) => poll_events(route),
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

fn set_rules(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "moderate") {
        return r;
    }
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let rules_val = match req.get("rules").and_then(Value::as_array) {
        Some(r) => r,
        None => return Reply::err(400, "invalid_rule"),
    };
    
    let mut parsed_rules = Vec::new();
    for r in rules_val {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let action = r.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        let priority = r.get("priority").and_then(Value::as_u64).unwrap_or(0) as u32;
        
        let effect_str = r.get("effect").and_then(Value::as_str).unwrap_or("");
        let effect = match effect_str {
            "allow" => policy::Effect::Allow,
            "deny" => policy::Effect::Deny,
            _ => return Reply::err(400, "invalid_rule"),
        };
        
        let conds_val = r.get("conditions").and_then(Value::as_array).unwrap_or(&vec![]).clone();
        let mut conditions = Vec::new();
        for c in conds_val {
            let left = c.get("left").and_then(Value::as_str).unwrap_or("").to_string();
            let right = c.get("right").and_then(Value::as_str).unwrap_or("").to_string();
            let op_str = c.get("op").and_then(Value::as_str).unwrap_or("");
            let op = match op_str {
                "eq" => policy::Op::Eq,
                "ne" => policy::Op::Ne,
                "in-list" => policy::Op::InList,
                "lt" => policy::Op::Lt,
                "gt" => policy::Op::Gt,
                "has" => policy::Op::Has,
                _ => return Reply::err(400, "invalid_rule"),
            };
            conditions.push(policy::Condition { left, op, right });
        }
        
        parsed_rules.push(policy::Rule {
            id,
            action,
            effect,
            conditions,
            priority
        });
    }

    match policy::set_rules(&cfg("policy-domain", "moderation"), &parsed_rules) {
        Ok(_) => Reply::no_content(),
        Err(_) => Reply::err(503, "policy_unavailable"),
    }
}

fn get_rules(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "moderate") {
        return r;
    }
    match policy::get_rules(&cfg("policy-domain", "moderation")) {
        Ok(rules) => {
            let json_rules: Vec<Value> = rules.into_iter().map(|r| {
                let effect_str = match r.effect {
                    policy::Effect::Allow => "allow",
                    policy::Effect::Deny => "deny",
                };
                let conds: Vec<Value> = r.conditions.into_iter().map(|c| {
                    let op_str = match c.op {
                        policy::Op::Eq => "eq",
                        policy::Op::Ne => "ne",
                        policy::Op::InList => "in-list",
                        policy::Op::Lt => "lt",
                        policy::Op::Gt => "gt",
                        policy::Op::Has => "has",
                    };
                    json!({
                        "left": c.left,
                        "op": op_str,
                        "right": c.right
                    })
                }).collect();
                json!({
                    "id": r.id,
                    "action": r.action,
                    "effect": effect_str,
                    "priority": r.priority,
                    "conditions": conds
                })
            }).collect();
            Reply::json(200, json!({"rules": json_rules}))
        }
        Err(_) => Reply::err(503, "policy_unavailable"),
    }
}

fn list_queue(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    
    let state = route.param("state");
    let state = if state.is_empty() { "pending".to_string() } else { state };
    let limit_str = route.param("limit");
    let limit = limit_str.parse::<u32>().unwrap_or(20).min(100) as usize;

    let mut entries = records::find_by("items", "state", &json!(state).to_string()).unwrap_or_default();
    
    entries.sort_by(|a, b| {
        let da: Value = serde_json::from_str(&a.data).unwrap_or(json!({}));
        let db: Value = serde_json::from_str(&b.data).unwrap_or(json!({}));
        let ta = da.get("submitted_at").and_then(Value::as_str).unwrap_or("");
        let tb = db.get("submitted_at").and_then(Value::as_str).unwrap_or("");
        ta.cmp(tb)
    });
    entries.truncate(limit);
    
    let items: Vec<Value> = entries.into_iter().map(|e| {
        let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
        if let Value::Object(ref mut m) = v {
            m.insert("id".to_string(), json!(e.id));
        }
        v
    }).collect();

    Reply::json(200, json!({"items": items}))
}

fn poll_events(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    let topic = route.param("topic");
    let topic = if topic.is_empty() { "moderation.decided".to_string() } else { topic };
    let max_str = route.param("max");
    let max = max_str.parse::<u32>().unwrap_or(20);
    
    match bus::poll(&topic, "queue-reader", max) {
        Ok(events) => {
            let json_events: Vec<Value> = events.into_iter().map(|e| {
                let payload = serde_json::from_slice::<Value>(&e.payload).unwrap_or(json!(null));
                json!({
                    "id": e.id,
                    "topic": e.topic,
                    "at": e.at,
                    "payload": payload
                })
            }).collect();
            Reply::json(200, json!({"events": json_events}))
        }
        Err(_) => Reply::err(503, "bus_unavailable"),
    }
}
