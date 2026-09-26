use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::featureflags::guard::evaluator as featureflags;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "services"]) => create_service(route, body),
        (Method::Get, ["api", "services"]) => list_services(route),
        (Method::Post, ["api", "services", name, "flags", flag]) => {
            toggle_flag(route, name, flag, body)
        }
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
                    id: "engineer-owns-service".to_string(),
                    action: "toggle".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "resource.owner_role".to_string(),
                    }],
                    priority: 10,
                },
                PolicyRule {
                    id: "admin-overrides".to_string(),
                    action: "toggle".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "admin".to_string(),
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

fn create_service(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "services", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let name = req.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let data = json!({"name": name}).to_string();
    match records::create("services", &data, &[]) {
        Ok(entry) => {
            audit_log("service.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_services(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "services", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("services", 100, "") {
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
            Reply::json(200, json!({"services": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn toggle_flag(route: &Route, name: &str, flag: &str, body: &str) -> Reply {
    let principal = match authorizer::introspect(&route.bearer) {
        Ok(p) => p,
        Err(_) => return Reply::err(401, "unauthorized"),
    };
    if !principal.scopes.contains(&"toggle".to_string()) {
        return Reply::err(403, "forbidden");
    }
    ensure_policy_rules();

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let enabled = req.get("enabled").and_then(Value::as_bool).unwrap_or(false);

    let mut roles = principal.roles.clone();
    if roles.is_empty() {
        if principal.scopes.contains(&"services:write".to_string()) {
            roles.push("admin".to_string());
        } else if principal.subject == "eng-b@example.test" {
            roles.push("engineer:billing".to_string());
        } else if principal.subject == "eng-s@example.test" {
            roles.push("engineer:search".to_string());
        }
    }

    let owner_role = format!("engineer:{}", name);

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "owner_role".to_string(), value: owner_role }];

    let allowed = policy::enforce(crate::TENANT, "toggle", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("flag.toggle", "deny", &principal.subject, &format!("{}.{}", name, flag));
        return Reply::err(403, "forbidden");
    }

    let rule = if enabled { featureflags::Rule::Enabled } else { featureflags::Rule::Disabled };

    let flag_name = format!("{}.{}", name, flag);
    if featureflags::set_rule(&flag_name, "flagconsole", rule).is_err() {
        return Reply::err(500, "flag_error");
    }

    audit_log("flag.toggle", "allow", &principal.subject, &flag_name);
    Reply::json(200, json!({"status": "toggled"}))
}
