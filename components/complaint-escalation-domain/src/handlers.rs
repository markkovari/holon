use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule};
use crate::bindings::records::store::store as records;
use crate::bindings::event::bus::bus;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "complaints"]) => create_complaint(route, body),
        (Method::Get, ["api", "complaints"]) => list_complaints(route),
        (Method::Post, ["api", "complaints", id, "assign"]) => assign(route, id, body),
        (Method::Post, ["api", "complaints", id, "escalate"]) => escalate(route, id),
        (Method::Post, ["api", "complaints", id, "resolve"]) => resolve(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, target: &str, action: &str) -> Result<Principal, Reply> {
    let perm = Permission { target: target.to_string(), action: action.to_string() };
    match authorizer::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p),
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(_) => Err(Reply::err(401, "unauthorized")),
    }
}

fn audit_log(event: &str, outcome: &str, subject: &str, detail: &str) {
    let _ = audit::record_event(&Event {
        id: String::new(),
        trace_id: String::new(),
        span_id: String::new(),
        timestamp: crate::now_secs(),
        event: event.to_string(),
        outcome: outcome.to_string(),
        tenant: crate::TENANT.to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
    });
}

fn ensure_policy_rules() {
    match records::find_by("meta", "kind", "policy_rules") {
        Ok(entries) if !entries.is_empty() => {}
        _ => {
            let rules = vec![
                Rule {
                    id: "assignee-acts".to_string(),
                    action: "*".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "resource.assignee".to_string(),
                        op: Op::Eq,
                        right: "principal.subject".to_string(),
                    }],
                    priority: 10,
                },
                Rule {
                    id: "supervisor-overrides".to_string(),
                    action: "*".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "supervisor".to_string(),
                    }],
                    priority: 5,
                },
            ];
            if policy::set_rules(crate::TENANT, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn create_complaint(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "complaints", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let summary = req.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
    let data = json!({"summary": summary, "status": "open", "assignee": ""}).to_string();
    match records::create("complaints", &data, &[]) {
        Ok(entry) => {
            audit_log("complaint.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_complaints(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "complaints", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("complaints", 100, "") {
        Ok(page) => {
            let items: Vec<Value> = page
                .entries
                .iter()
                .map(|e| {
                    let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
                    if let Value::Object(ref mut m) = v {
                        m.insert("id".to_string(), json!(e.id));
                    }
                    v
                })
                .collect();
            Reply::json(200, json!({"complaints": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn assign(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match authorize_perm(route, "complaints", "assign") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let agent = req.get("agent").and_then(Value::as_str).unwrap_or("").to_string();
    
    let entry = match records::get("complaints", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut complaint: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    complaint["status"] = json!("assigned");
    complaint["assignee"] = json!(agent.clone());
    if records::update("complaints", id, &complaint.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }
    audit_log("complaint.assign", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "assigned", "assignee": agent}))
}

fn act_on_complaint(route: &Route, id: &str, action: &str, new_status: &str) -> Reply {
    let principal = match authorize_perm(route, "complaints", action) {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("complaints", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut complaint: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let assignee = complaint.get("assignee").and_then(Value::as_str).unwrap_or("");
    if assignee.is_empty() {
        audit_log(&format!("complaint.{}", action), "deny", &principal.subject, id);
        return Reply::err(409, "not_assigned");
    }

    let mut roles = principal.roles.clone();
    if roles.is_empty() && principal.scopes.contains(&"complaints:assign".to_string()) {
        roles.push("supervisor".to_string());
    }

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "assignee".to_string(), value: assignee.to_string() }];

    let allowed = policy::enforce(crate::TENANT, action, &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log(&format!("complaint.{}", action), "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    complaint["status"] = json!(new_status);
    if records::update("complaints", id, &complaint.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    if action == "resolve" {
        let _ = bus::publish("complaint.resolved", id.as_bytes());
    }

    audit_log(&format!("complaint.{}", action), "allow", &principal.subject, id);
    Reply::json(200, json!({"status": new_status}))
}

fn escalate(route: &Route, id: &str) -> Reply {
    act_on_complaint(route, id, "escalate", "escalated")
}

fn resolve(route: &Route, id: &str) -> Reply {
    act_on_complaint(route, id, "resolve", "resolved")
}
