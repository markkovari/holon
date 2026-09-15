use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule};
use crate::bindings::records::store::store as records;
use crate::bindings::quota::meter::meter;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "rooms"]) => create_room(route, body),
        (Method::Get, ["api", "rooms"]) => list_rooms(route),
        (Method::Post, ["api", "rooms", id, "book"]) => book(route, id, body),
        (Method::Post, ["api", "bookings", id, "cancel"]) => cancel(route, id),
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
                    id: "booker-cancels".to_string(),
                    action: "cancel".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "resource.subject".to_string(),
                        op: Op::Eq,
                        right: "principal.subject".to_string(),
                    }],
                    priority: 10,
                },
                Rule {
                    id: "admin-overrides".to_string(),
                    action: "cancel".to_string(),
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

fn create_room(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "rooms", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let name = req.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let data = json!({"name": name}).to_string();
    match records::create("rooms", &data, &[]) {
        Ok(entry) => {
            audit_log("room.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_rooms(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "rooms", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("rooms", 100, "") {
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
            Reply::json(200, json!({"rooms": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn book(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match authorize_perm(route, "bookings", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    
    let _room = match records::get("rooms", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let date = req.get("date").and_then(Value::as_str).unwrap_or("").to_string();

    if let Ok(page) = records::list_records("bookings", 100, "") {
        for entry in page.entries {
            let booking: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
            if booking["room_id"] == json!(id) && booking["date"] == json!(date) && booking["cancelled"] != json!(true) {
                audit_log("booking.create", "deny", &principal.subject, id);
                return Reply::err(409, "already_booked");
            }
        }
    }

    if meter::reserve(&principal.subject, 1, 2, 86400).is_err() {
        audit_log("booking.create", "deny", &principal.subject, id);
        return Reply::err(429, "quota_exceeded");
    }

    let data = json!({
        "room_id": id,
        "date": date,
        "subject": principal.subject,
        "cancelled": false
    }).to_string();

    match records::create("bookings", &data, &["room_id".to_string()]) {
        Ok(entry) => {
            audit_log("booking.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn cancel(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "bookings", "write") { // Wait, the scope is bookings:write
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("bookings", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut booking: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let resource_subject = booking.get("subject").and_then(Value::as_str).unwrap_or("").to_string();

    let mut roles = principal.roles.clone();
    if roles.is_empty() && principal.scopes.contains(&"rooms:write".to_string()) {
        roles.push("admin".to_string());
    }

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "subject".to_string(), value: resource_subject }];

    let allowed = policy::enforce(crate::TENANT, "cancel", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("booking.cancel", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    booking["cancelled"] = json!(true);
    if records::update("bookings", id, &booking.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    audit_log("booking.cancel", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "cancelled"}))
}
