//! Projects (shared) and time entries (`pending -> approved`/`rejected`). A
//! `member` logs and edits only their own entries; a `manager` approves only
//! entries routed to THEM (the `manager` field the member names when logging
//! one) — the row-level rule `auth:identity/rbac` cannot express, enforced
//! with `policy:guard` exactly as `crm-domain`/`ats-domain` enforce their own
//! ownership rule.

use crate::bindings::auth::identity::types::Principal;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_manager, Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "entries";

guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "projects"]) => create_project(route, body),
        (Method::Get, ["api", "projects"]) => list_projects(route),
        (Method::Post, ["api", "entries"]) => create_entry(route, body),
        (Method::Get, ["api", "entries"]) => list_entries(route),
        (Method::Post, ["api", "entries", id, "approve"]) => decide_entry(route, id, "approved"),
        (Method::Post, ["api", "entries", id, "reject"]) => decide_entry(route, id, "rejected"),
        _ => Reply::err(404, "not_found"),
    }
}

/// Idempotent: ONLY the manager an entry is routed to may decide it —
/// deliberately not the member who logged it (self-approval would defeat
/// the point of routing it to someone else at all).
fn ensure_policy_rules() {
    match records::find_by("meta", "kind", "\"policy_rules\"") {
        Ok(entries) if !entries.is_empty() => {}
        _ => {
            let rules = vec![PolicyRule {
                id: "routed-manager-decides".to_string(),
                action: "decide".to_string(),
                effect: Effect::Allow,
                conditions: vec![Condition {
                    left: "resource.manager".to_string(),
                    op: Op::Eq,
                    right: "principal.subject".to_string(),
                }],
                priority: 5,
            }];
            if policy::set_rules(POLICY_DOMAIN, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn enforce(action: &str, p: &Principal, manager: &str) -> bool {
    ensure_policy_rules();
    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: p.subject.clone() },
        Attr { key: "roles".to_string(), value: p.roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "manager".to_string(), value: manager.to_string() }];
    policy::enforce(POLICY_DOMAIN, action, &principal_attrs, &resource_attrs)
}

#[derive(serde::Deserialize)]
struct ProjectReq {
    #[serde(default)]
    name: String,
}

fn create_project(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, ProjectReq);
    if req.name.is_empty() {
        return Reply::err(400, "name is required");
    }
    let data = json!({"name": req.name}).to_string();
    match records::create("projects", &data, &[]) {
        Ok(entry) => {
            audit("project.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_projects(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    match records::list_records("projects", 100, "") {
        Ok(page) => Reply::json(200, json!({"projects": entries_json(&page.entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct EntryReq {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    hours: f64,
    #[serde(default)]
    note: String,
    /// The manager's subject id this entry is routed to for approval (from
    /// their `/register` or `/me` response).
    #[serde(default)]
    manager: String,
}

fn create_entry(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, EntryReq);
    if req.project_id.is_empty() || req.hours <= 0.0 {
        return Reply::err(400, "project_id and a positive hours value are required");
    }
    let data = json!({
        "project_id": req.project_id,
        "date": req.date,
        "hours": req.hours,
        "note": req.note,
        "status": "pending",
        "owner": principal.subject,
        "manager": req.manager,
    })
    .to_string();
    match records::create("entries", &data, &["owner".to_string(), "manager".to_string()]) {
        Ok(entry) => {
            audit("entry.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// A member sees their own entries; a manager sees the ones routed to them —
/// two different equals-filters on the same collection, never both at once.
fn list_entries(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let subject_json = serde_json::to_string(&principal.subject).unwrap_or_default();
    let result = if is_manager(&principal) {
        records::find_by("entries", "manager", &subject_json)
    } else {
        records::find_by("entries", "owner", &subject_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"entries": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn decide_entry(route: &Route, id: &str, new_status: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("entries", id);
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let manager = data.get("manager").and_then(Value::as_str).unwrap_or("").to_string();
    guestauth::guest_deny_unless!(
        enforce("decide", &principal, &manager),
        principal,
        "entry.decide",
        id
    );
    data["status"] = json!(new_status);
    match records::update("entries", id, &data.to_string(), entry.revision) {
        Ok(_) => {
            audit("entry.decide", "allow", &principal.subject, &format!("{id} -> {new_status}"));
            Reply::json(200, json!({"id": id, "status": new_status}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}
