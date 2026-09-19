//! Expense reports (`submitted -> approved/rejected`). An `employee` sees
//! only reports they filed — the row-level rule `auth:identity/rbac` cannot
//! express, enforced with `policy:guard` exactly as `billing-domain`/
//! `crm-domain` enforce their own ownership rule. Approving or rejecting a
//! report is `admin`-only, checked directly against `principal.roles` — a
//! role, not a row, decides that one (mirrors `billing-domain::pay_invoice`).
//!
//! GOAL (`.comp/goals/expense-report.toml`): this file is UNIMPLEMENTED. The
//! five handlers below return `501`. The dispatch table, the ABAC helper
//! (`owns_or_admin`, from `guest_owner_or_admin_policy!` below), and every
//! import already wired are not the part being asked for — fill in the
//! bodies.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "reports";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "employee");

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
#[allow(dead_code)]
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
    let _ = (route, body);
    Reply::err(501, "not_implemented: create a report with status \"submitted\", employee = caller's subject")
}

/// `admin` sees every report; an `employee` sees only their own
/// (`records::find_by("reports", "employee", ...)`, the same shape as
/// `billing-domain::list_invoices`).
fn list_reports(route: &Route) -> Reply {
    let _ = route;
    Reply::err(501, "not_implemented: admin sees every report, employee sees only their own")
}

/// `owns_or_admin("view", ...)` gates this — the report's own `employee`, or
/// an admin.
fn get_report(route: &Route, id: &str) -> Reply {
    let _ = (route, id);
    Reply::err(501, "not_implemented: owns_or_admin gates this by the report's employee field")
}

/// `admin`-only. `submitted -> approved`; refuse a report not currently
/// `submitted`.
fn approve_report(route: &Route, id: &str) -> Reply {
    let _ = (route, id);
    Reply::err(501, "not_implemented: admin-only, submitted -> approved")
}

/// `admin`-only. `submitted -> rejected`; refuse a report not currently
/// `submitted`.
fn reject_report(route: &Route, id: &str) -> Reply {
    let _ = (route, id);
    Reply::err(501, "not_implemented: admin-only, submitted -> rejected")
}

/// The stored document with the store's id merged in — same helper
/// `billing-domain`/`crm-domain` each carry.
#[allow(dead_code)]
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
