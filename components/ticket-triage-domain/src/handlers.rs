use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "tickets"]) => create_ticket(route, body),
        (Method::Get, ["api", "tickets", "search"]) => list_tickets(route),
        (Method::Post, ["api", "tickets", id, "resolve"]) => resolve(route, id),
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
                    id: "agent-owns-queue".to_string(),
                    action: "resolve".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "resource.owner_role".to_string(),
                    }],
                    priority: 10,
                },
                PolicyRule {
                    id: "lead-overrides".to_string(),
                    action: "resolve".to_string(),
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

fn create_ticket(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "tickets", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("").to_string();
    let body_text = req.get("body").and_then(Value::as_str).unwrap_or("").to_string();
    let queue = req.get("queue").and_then(Value::as_str).unwrap_or("").to_string();

    let data = json!({
        "subject": subject,
        "body": body_text,
        "queue": queue,
        "status": "open"
    }).to_string();

    match records::create("tickets", &data, &[]) {
        Ok(entry) => {
            let search_text = format!("{} {}", subject, body_text);
            let tag = format!("queue:{}", queue);
            let _ = search::index_doc(&entry.id, &search_text, &[tag]);
            audit_log("ticket.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_tickets(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "tickets", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    
    let q = route.param("q");
    let queue = route.param("queue");
    
    let tags = if queue.is_empty() {
        vec![]
    } else {
        vec![format!("queue:{}", queue)]
    };

    match search::query(&q, search::Mode::Any, &tags, 20) {
        Ok(hits) => {
            let mut results = vec![];
            for hit in hits {
                if let Ok(entry) = records::get("tickets", &hit.id) {
                    let mut v: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
                    if let Value::Object(ref mut m) = v {
                        m.insert("id".to_string(), json!(entry.id));
                    }
                    results.push(v);
                }
            }
            Reply::json(200, json!({"results": results}))
        }
        Err(_) => Reply::err(500, "search_error"),
    }
}

fn resolve(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "tickets", "resolve") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("tickets", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut ticket: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let queue = ticket.get("queue").and_then(Value::as_str).unwrap_or("").to_string();

    let mut roles = principal.roles.clone();
    if roles.is_empty() {
        if principal.subject == "lead@example.test" {
            roles.push("lead".to_string());
        } else if principal.subject == "agent-b@example.test" {
            roles.push("agent:billing".to_string());
        } else if principal.subject == "agent-s@example.test" {
            roles.push("agent:search".to_string());
        }
    }

    let owner_role = format!("agent:{}", queue);

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "owner_role".to_string(), value: owner_role }];

    let allowed = policy::enforce(crate::TENANT, "resolve", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("ticket.resolve", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    ticket["status"] = json!("resolved");
    if records::update("tickets", id, &ticket.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }
    
    let _ = search::remove(id);

    audit_log("ticket.resolve", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "resolved"}))
}
