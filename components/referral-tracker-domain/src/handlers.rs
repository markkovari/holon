use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::notify::dispatch::dispatcher as notify;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "postings"]) => create_posting(route, body),
        (Method::Get, ["api", "postings"]) => list_postings(route),
        (Method::Post, ["api", "postings", id, "referrals"]) => make_referral(route, id, body),
        (Method::Post, ["api", "postings", id, "close"]) => close_posting(route, id),
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
            let rules = vec![PolicyRule {
                id: "owner-closes".to_string(),
                action: "close".to_string(),
                effect: Effect::Allow,
                conditions: vec![Condition {
                    left: "resource.owner".to_string(),
                    op: Op::Eq,
                    right: "principal.subject".to_string(),
                }],
                priority: 10,
            }];
            if policy::set_rules(crate::TENANT, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn create_posting(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "postings", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let title = req.get("title").and_then(Value::as_str).unwrap_or("").to_string();

    let data = json!({
        "title": title,
        "owner": principal.subject.clone(),
        "status": "open"
    })
    .to_string();

    match records::create("postings", &data, &[]) {
        Ok(entry) => {
            audit_log("posting.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_postings(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "postings", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("postings", 100, "") {
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
            Reply::json(200, json!({"postings": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn make_referral(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match authorize_perm(route, "referrals", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };

    let entry = match records::get("postings", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let posting: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let title = posting.get("title").and_then(Value::as_str).unwrap_or("").to_string();

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let candidate_email =
        req.get("candidate_email").and_then(Value::as_str).unwrap_or("").to_string();

    let data = json!({
        "posting_id": id,
        "referred_by": principal.subject.clone(),
        "candidate_email": candidate_email
    })
    .to_string();

    match records::create("referrals", &data, &[]) {
        Ok(ref_entry) => {
            let msg = notify::Message {
                channel: notify::Channel::Webhook,
                target: "https://example.test/notify".to_string(),
                subject: "".to_string(),
                body: format!("new referral for {}", title),
            };
            let _ = notify::send(&msg);

            audit_log("referral.create", "allow", &principal.subject, &ref_entry.id);
            Reply::json(201, json!({"id": ref_entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn close_posting(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "postings", "close") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("postings", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut posting: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let owner = posting.get("owner").and_then(Value::as_str).unwrap_or("").to_string();

    let principal_attrs =
        vec![Attr { key: "subject".to_string(), value: principal.subject.clone() }];
    let resource_attrs = vec![Attr { key: "owner".to_string(), value: owner }];

    let allowed = policy::enforce(crate::TENANT, "close", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("posting.close", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    posting["status"] = json!("closed");
    if records::update("postings", id, &posting.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    audit_log("posting.close", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "closed"}))
}
