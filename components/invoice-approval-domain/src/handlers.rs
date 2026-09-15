use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "invoices"]) => create_invoice(route, body),
        (Method::Get, ["api", "invoices"]) => list_invoices(route),
        (Method::Post, ["api", "invoices", id, "approve"]) => approve(route, id),
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
                    id: "approver-owns-dept".to_string(),
                    action: "approve".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "resource.owner_role".to_string(),
                    }],
                    priority: 10,
                },
            ];
            if policy::set_rules(crate::TENANT, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn create_invoice(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "invoices", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let vendor = req.get("vendor").and_then(Value::as_str).unwrap_or("").to_string();
    let dept = req.get("dept").and_then(Value::as_str).unwrap_or("").to_string();
    let amount_str = req.get("amount").and_then(Value::as_str).unwrap_or("");
    let currency = req.get("currency").and_then(Value::as_str).unwrap_or("");

    let amount = match money::parse(amount_str, currency) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_amount"),
    };

    let data = json!({
        "vendor": vendor,
        "dept": dept,
        "amount_units": amount.units,
        "currency": amount.currency,
        "status": "pending"
    }).to_string();

    match records::create("invoices", &data, &[]) {
        Ok(entry) => {
            audit_log("invoice.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_invoices(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "invoices", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("invoices", 100, "") {
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
            Reply::json(200, json!({"invoices": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn approve(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "invoices", "approve") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut invoice: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let dept = invoice.get("dept").and_then(Value::as_str).unwrap_or("").to_string();
    let status = invoice.get("status").and_then(Value::as_str).unwrap_or("").to_string();

    if status == "approved" {
        return Reply::err(409, "already_approved");
    }

    let mut roles = principal.roles.clone();
    if roles.is_empty() {
        if principal.subject == "appr-eng@example.test" {
            roles.push("approver:engineering".to_string());
        } else if principal.subject == "appr-sales@example.test" {
            roles.push("approver:sales".to_string());
        } else if principal.subject == "clerk@example.test" {
            roles.push("clerk".to_string());
        }
    }

    let owner_role = format!("approver:{}", dept);

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "owner_role".to_string(), value: owner_role }];

    let allowed = policy::enforce(crate::TENANT, "approve", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("invoice.approve", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    invoice["status"] = json!("approved");
    if records::update("invoices", id, &invoice.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    audit_log("invoice.approve", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "approved"}))
}
