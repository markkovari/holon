//! `volunteer-shift-domain` — the goal.
//!
//! See the goal text in `.comp/goals/volunteer-shift-domain.toml` for the routes, the storage
//! shape, the row-level policy:guard rule, and the one extra capability this
//! app composes.

use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule};
use crate::bindings::records::store::store as records;
use crate::bindings::quota::meter::meter as quota;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "shifts"]) => create_shift(route, body),
        (Method::Get, ["api", "shifts"]) => list_shifts(route),
        (Method::Post, ["api", "shifts", shift_id, "signup"]) => signup(route, shift_id),
        (Method::Post, ["api", "shifts", _shift_id, "signups", signup_id, "cancel"]) => {
            cancel(route, signup_id)
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
                Rule {
                    id: "signup-owner-cancels".to_string(),
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
                    id: "coordinator-overrides".to_string(),
                    action: "cancel".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "coordinator".to_string(),
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
    let title = req.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let slots = req.get("slots").and_then(Value::as_u64).unwrap_or(0) as u32;
    let data = json!({"title": title, "slots": slots, "filled": 0}).to_string();
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

fn signup(route: &Route, shift_id: &str) -> Reply {
    let principal = match authorize_perm(route, "signups", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let entry = match records::get("shifts", shift_id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut shift: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let slots = shift.get("slots").and_then(Value::as_u64).unwrap_or(0);
    let filled = shift.get("filled").and_then(Value::as_u64).unwrap_or(0);
    if filled >= slots {
        audit_log("shift.signup", "deny", &principal.subject, shift_id);
        return Reply::err(409, "no_slots");
    }

    match quota::reserve(&principal.subject, 1, 3, 604800) {
        Ok(_) => {}
        Err(quota::QuotaError::Exceeded(_)) => {
            audit_log("shift.signup", "deny", &principal.subject, shift_id);
            return Reply::err(429, "quota_exceeded");
        }
        Err(_) => return Reply::err(503, "quota_unavailable"),
    }

    let new_filled = filled + 1;
    shift["filled"] = json!(new_filled);
    if records::update("shifts", shift_id, &shift.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    let signup_data = json!({
        "shift_id": shift_id,
        "subject": principal.subject,
        "at": crate::now_secs(),
    })
    .to_string();
    match records::create("signups", &signup_data, &["shift_id".to_string()]) {
        Ok(sentry) => {
            audit_log("shift.signup", "allow", &principal.subject, &sentry.id);
            Reply::json(201, json!({"signup_id": sentry.id, "id": sentry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn cancel(route: &Route, signup_id: &str) -> Reply {
    let principal = match authorize_perm(route, "signups", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let sentry = match records::get("signups", signup_id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let signup_json: Value = serde_json::from_str(&sentry.data).unwrap_or(json!({}));
    let resource_subject = signup_json.get("subject").and_then(Value::as_str).unwrap_or("").to_string();

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: principal.roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "subject".to_string(), value: resource_subject }];

    let allowed = policy::enforce(crate::TENANT, "cancel", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("signup.cancel", "deny", &principal.subject, signup_id);
        return Reply::err(403, "forbidden");
    }

    if let Some(shift_id) = signup_json.get("shift_id").and_then(Value::as_str) {
        if let Ok(shift_entry) = records::get("shifts", shift_id) {
            let mut shift: Value = serde_json::from_str(&shift_entry.data).unwrap_or(json!({}));
            let filled = shift.get("filled").and_then(Value::as_u64).unwrap_or(0);
            let new_filled = if filled > 0 { filled - 1 } else { 0 };
            shift["filled"] = json!(new_filled);
            let _ = records::update("shifts", shift_id, &shift.to_string(), shift_entry.revision);
        }
    }

    let mut updated_signup = signup_json.clone();
    updated_signup["cancelled"] = json!(true);
    let _ = records::update("signups", signup_id, &updated_signup.to_string(), sentry.revision);

    audit_log("signup.cancel", "allow", &principal.subject, signup_id);
    Reply::json(200, json!({"cancelled": true}))
}