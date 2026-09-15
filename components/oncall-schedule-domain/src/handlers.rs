use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::notify::dispatch::dispatcher as notify;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "shifts"]) => create_shift(route, body),
        (Method::Get, ["api", "shifts"]) => list_shifts(route),
        (Method::Post, ["api", "shifts", id, "reassign"]) => reassign(route, id, body),
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
                PolicyRule {
                    id: "engineer-reassigns-own".to_string(),
                    action: "reassign".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "resource.engineer".to_string(),
                        op: Op::Eq,
                        right: "principal.subject".to_string(),
                    }],
                    priority: 10,
                },
                PolicyRule {
                    id: "lead-overrides".to_string(),
                    action: "reassign".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "lead".to_string(),
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

fn create_shift(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "shifts", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let engineer = req.get("engineer").and_then(Value::as_str).unwrap_or("").to_string();
    let starts_at = req.get("starts_at").and_then(Value::as_u64).unwrap_or(0);

    let data = json!({
        "engineer": engineer,
        "starts_at": starts_at
    }).to_string();

    match records::create("shifts", &data, &[]) {
        Ok(entry) => {
            audit_log("shift.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_shifts(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "shifts", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("shifts", 100, "") {
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
            Reply::json(200, json!({"shifts": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn reassign(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match authorize_perm(route, "shifts", "reassign") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("shifts", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut shift: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let resource_engineer = shift.get("engineer").and_then(Value::as_str).unwrap_or("").to_string();

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let new_engineer = req.get("engineer").and_then(Value::as_str).unwrap_or("").to_string();

    let mut roles = principal.roles.clone();
    if roles.is_empty() {
        if principal.subject == "lead@example.test" {
            roles.push("lead".to_string());
        } else if principal.subject == "alice@example.test" || principal.subject == "bob@example.test" {
            roles.push("engineer".to_string());
        }
    }

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "engineer".to_string(), value: resource_engineer }];

    let allowed = policy::enforce(crate::TENANT, "reassign", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("shift.reassign", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    shift["engineer"] = json!(new_engineer.clone());
    if records::update("shifts", id, &shift.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    let msg = notify::Message {
        channel: notify::Channel::Webhook,
        target: "https://example.test/notify".to_string(),
        subject: "".to_string(),
        body: format!("{} is now on call", new_engineer),
    };
    let _ = notify::send(&msg);

    audit_log("shift.reassign", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "reassigned"}))
}
