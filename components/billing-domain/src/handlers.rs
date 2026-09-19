//! Clients + invoices (`draft -> sent -> paid`, or `-> overdue`). A `biller`
//! sees only invoices they created — the row-level rule `auth:identity/rbac`
//! cannot express, enforced with `policy:guard` exactly as
//! `crm-domain`/`ticket-triage-domain` enforce their own ownership rule.
//! Marking an invoice paid is `admin`-only, checked directly against
//! `principal.roles` — a role, not a row, decides that one.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "invoices";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "owner");
guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "clients"]) => create_client(route, body),
        (Method::Get, ["api", "clients"]) => list_clients(route),
        (Method::Post, ["api", "invoices"]) => create_invoice(route, body),
        (Method::Get, ["api", "invoices"]) => list_invoices(route),
        (Method::Post, ["api", "invoices", id, "send"]) => send_invoice(route, id),
        (Method::Post, ["api", "invoices", id, "pay"]) => pay_invoice(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

#[derive(serde::Deserialize)]
struct ClientReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

fn create_client(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req: ClientReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.name.is_empty() {
        return Reply::err(400, "name is required");
    }
    let data = json!({"name": req.name, "email": req.email}).to_string();
    match records::create("clients", &data, &[]) {
        Ok(entry) => {
            audit("client.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_clients(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    match records::list_records("clients", 100, "") {
        Ok(page) => Reply::json(200, json!({"clients": entries_json(&page.entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct LineItem {
    #[serde(default)]
    description: String,
    #[serde(default)]
    amount: f64,
}

#[derive(serde::Deserialize)]
struct InvoiceReq {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    line_items: Vec<LineItem>,
}

fn create_invoice(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req: InvoiceReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.client_id.is_empty() || req.line_items.is_empty() {
        return Reply::err(400, "client_id and at least one line item are required");
    }
    let total: f64 = req.line_items.iter().map(|i| i.amount).sum();
    let items: Vec<Value> =
        req.line_items.iter().map(|i| json!({"description": i.description, "amount": i.amount})).collect();
    let data = json!({
        "client_id": req.client_id,
        "line_items": items,
        "total": total,
        "status": "draft",
        "owner": principal.subject,
    })
    .to_string();
    match records::create("invoices", &data, &["owner".to_string()]) {
        Ok(entry) => {
            audit("invoice.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id, "total": total}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_invoices(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let result = if is_admin(&principal) {
        records::list_records("invoices", 100, "").map(|p| p.entries)
    } else {
        let owner_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        records::find_by("invoices", "owner", &owner_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"invoices": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn send_invoice(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut inv: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let owner = inv.get("owner").and_then(Value::as_str).unwrap_or("").to_string();
    guestauth::guest_deny_unless!(owns_or_admin("edit", &principal, &owner), principal, "invoice.send", id);
    if inv.get("status").and_then(Value::as_str) != Some("draft") {
        return Reply::err(400, "only a draft invoice can be sent");
    }
    inv["status"] = json!("sent");
    match records::update("invoices", id, &inv.to_string(), entry.revision) {
        Ok(_) => {
            audit("invoice.send", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "status": "sent"}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

/// Admin-only, checked directly against the role — unlike `send`, this isn't
/// about who OWNS the invoice, it's about who is trusted to confirm money
/// arrived.
fn pay_invoice(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "invoice.pay", id);
    let entry = match records::get("invoices", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut inv: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    inv["status"] = json!("paid");
    match records::update("invoices", id, &inv.to_string(), entry.revision) {
        Ok(_) => {
            audit("invoice.pay", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "status": "paid"}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}
