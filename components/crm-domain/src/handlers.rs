//! Contacts + a deal pipeline (`lead -> qualified -> proposal -> won/lost`).
//! `admin` sees and edits every deal; `rep` only their own — the row-level
//! rule `auth:identity/rbac` cannot express, enforced here with
//! `policy:guard` exactly as `ticket-triage-domain` enforces "resolve only
//! your own queue's ticket".

use crate::bindings::auth::identity::types::Principal;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const STAGES: &[&str] = &["lead", "qualified", "proposal", "won", "lost"];
const POLICY_DOMAIN: &str = "deals";

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "contacts"]) => create_contact(route, body),
        (Method::Get, ["api", "contacts"]) => list_contacts(route),
        (Method::Post, ["api", "deals"]) => create_deal(route, body),
        (Method::Get, ["api", "deals"]) => list_deals(route),
        (Method::Post, ["api", "deals", id, "stage"]) => move_stage(route, id, body),
        (Method::Post, ["api", "deals", id, "notes"]) => add_note(route, id, body),
        _ => Reply::err(404, "not_found"),
    }
}

/// Idempotent: define the one row-level rule this app needs. A rep may act
/// on a deal they own; an admin may act on any.
fn ensure_policy_rules() {
    match records::find_by("meta", "kind", "\"policy_rules\"") {
        Ok(entries) if !entries.is_empty() => {}
        _ => {
            let rules = vec![
                PolicyRule {
                    id: "owner-may-act".to_string(),
                    action: "*".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "resource.owner".to_string(),
                        op: Op::Eq,
                        right: "principal.subject".to_string(),
                    }],
                    priority: 10,
                },
                PolicyRule {
                    id: "admin-may-act".to_string(),
                    action: "*".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "admin".to_string(),
                    }],
                    priority: 5,
                },
            ];
            if policy::set_rules(POLICY_DOMAIN, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn owns_or_admin(action: &str, p: &Principal, owner: &str) -> bool {
    ensure_policy_rules();
    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: p.subject.clone() },
        Attr { key: "roles".to_string(), value: p.roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "owner".to_string(), value: owner.to_string() }];
    policy::enforce(POLICY_DOMAIN, action, &principal_attrs, &resource_attrs)
}

#[derive(serde::Deserialize)]
struct ContactReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    company: String,
}

fn create_contact(route: &Route, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: ContactReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.name.is_empty() {
        return Reply::err(400, "name is required");
    }
    let data = json!({
        "name": req.name, "email": req.email, "phone": req.phone, "company": req.company,
    })
    .to_string();
    match records::create("contacts", &data, &[]) {
        Ok(entry) => {
            audit("contact.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_contacts(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    match records::list_records("contacts", 100, "") {
        Ok(page) => Reply::json(200, json!({"contacts": entries_json(&page.entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct DealReq {
    #[serde(default)]
    contact_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    amount: f64,
}

fn create_deal(route: &Route, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: DealReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.title.is_empty() {
        return Reply::err(400, "title is required");
    }
    let data = json!({
        "contact_id": req.contact_id,
        "title": req.title,
        "amount": req.amount,
        "stage": "lead",
        "owner": principal.subject,
        "notes": [],
    })
    .to_string();
    match records::create("deals", &data, &["owner".to_string()]) {
        Ok(entry) => {
            audit("deal.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_deals(route: &Route) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = if is_admin(&principal) {
        records::list_records("deals", 100, "").map(|p| p.entries)
    } else {
        let owner_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        records::find_by("deals", "owner", &owner_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"deals": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn move_stage(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let entry = match records::get("deals", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut deal: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let owner = deal.get("owner").and_then(Value::as_str).unwrap_or("").to_string();
    if !owns_or_admin("edit", &principal, &owner) {
        audit("deal.stage", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let stage = req.get("stage").and_then(Value::as_str).unwrap_or("");
    if !STAGES.contains(&stage) {
        return Reply::err(400, "invalid stage");
    }
    deal["stage"] = json!(stage);
    match records::update("deals", id, &deal.to_string(), entry.revision) {
        Ok(_) => {
            audit("deal.stage", "allow", &principal.subject, &format!("{id} -> {stage}"));
            Reply::json(200, json!({"id": id, "stage": stage}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

fn add_note(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let entry = match records::get("deals", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut deal: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let owner = deal.get("owner").and_then(Value::as_str).unwrap_or("").to_string();
    if !owns_or_admin("edit", &principal, &owner) {
        return Reply::err(403, "forbidden");
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let text = req.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    if text.is_empty() {
        return Reply::err(400, "text is required");
    }
    let notes = deal["notes"].as_array_mut().map(|a| a.push(json!(text)));
    if notes.is_none() {
        deal["notes"] = json!([text]);
    }
    match records::update("deals", id, &deal.to_string(), entry.revision) {
        Ok(_) => {
            audit("deal.note", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

fn entries_json(entries: &[records::Entry]) -> Vec<Value> {
    entries
        .iter()
        .map(|e| {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            v
        })
        .collect()
}
