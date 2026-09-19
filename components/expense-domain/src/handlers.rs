//! Expense reports (`submitted -> approved/rejected`). An `employee` sees
//! only reports they filed — the row-level rule `auth:identity/rbac` cannot
//! express, enforced with `policy:guard` exactly as `billing-domain`/
//! `crm-domain` enforce their own ownership rule. Approving or rejecting a
//! report is `admin`-only, checked directly against `principal.roles` — a
//! role, not a row, decides that one (mirrors `billing-domain::pay_invoice`).

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "reports";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "employee");
guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reports"]) => create_report(route, body),
        (Method::Get, ["api", "reports"]) => list_reports(route),
        (Method::Get, ["api", "reports", id]) => get_report(route, id),
        (Method::Post, ["api", "reports", id, "approve"]) => approve_report(route, id),
        (Method::Post, ["api", "reports", id, "reject"]) => reject_report(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

/// `{"amount": <u32 cents>, "note": <string>}`.
#[derive(serde::Deserialize)]
struct ReportReq {
    #[serde(default)]
    amount: u32,
    #[serde(default)]
    note: String,
}

/// Any authenticated user (employee or admin) files their own report.
/// Stores it in the `reports` collection with `status: "submitted"` and
/// `employee` set to the caller's subject.
fn create_report(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, ReportReq);
    if req.amount == 0 {
        return Reply::err(400, "amount is required");
    }
    let data = json!({
        "employee": principal.subject,
        "amount": req.amount,
        "note": req.note,
        "status": "submitted",
    })
    .to_string();
    match records::create("reports", &data, &["employee".to_string()]) {
        Ok(entry) => {
            audit("report.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id, "status": "submitted"}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// `admin` sees every report; an `employee` sees only their own
/// (`records::find_by("reports", "employee", ...)`, the same shape as
/// `billing-domain::list_invoices`).
fn list_reports(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let result = if is_admin(&principal) {
        records::list_records("reports", 100, "").map(|p| p.entries)
    } else {
        let employee_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        records::find_by("reports", "employee", &employee_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"reports": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// `owns_or_admin("view", ...)` gates this — the report's own `employee`, or
/// an admin.
fn get_report(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("reports", id);
    let mut report: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let employee = report.get("employee").and_then(Value::as_str).unwrap_or("").to_string();
    guestauth::guest_deny_unless!(owns_or_admin("view", &principal, &employee), principal, "report.view", id);
    if let Value::Object(ref mut m) = report {
        m.insert("id".to_string(), json!(entry.id));
    }
    Reply::json(200, report)
}

/// `admin`-only. `submitted -> approved`; refuse a report not currently
/// `submitted`.
fn approve_report(route: &Route, id: &str) -> Reply {
    transition(route, id, "approved", "report.approve")
}

/// `admin`-only. `submitted -> rejected`; refuse a report not currently
/// `submitted`.
fn reject_report(route: &Route, id: &str) -> Reply {
    transition(route, id, "rejected", "report.reject")
}

/// The shared `submitted -> <other>` move. Admin-only, checked directly
/// against the role — unlike `get`/`list`, this isn't about who OWNS the
/// report (even the employee who filed it cannot approve their own), it's
/// about who is trusted to sign off on the money.
fn transition(route: &Route, id: &str, next: &str, action: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, action, id);
    let entry = guestauth::guest_get_or_404!("reports", id);
    let mut report: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    if report.get("status").and_then(Value::as_str) != Some("submitted") {
        return Reply::err(400, "only a submitted report can be approved or rejected");
    }
    report["status"] = json!(next);
    match records::update("reports", id, &report.to_string(), entry.revision) {
        Ok(_) => {
            audit(action, "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "status": next}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}
