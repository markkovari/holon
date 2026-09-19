//! Part 2 — payments, shipments, returns.

use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::ledger::doubleentry::ledger;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, is_admin, Reply, Route};
use serde_json::{json, Value};

/// The authenticated principal, or an early `401` — wraps the same
/// `crate::introspect` the router's auth macros build on.
macro_rules! authenticated {
    ($route:expr) => {
        match crate::introspect($route) {
            Ok(principal) => principal,
            Err(_) => return Reply::err(401, "unauthorized"),
        }
    };
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "orders", id, "pay"]) => pay(route, id),
        (Method::Post, ["api", "orders", id, "ship"]) => ship(route, id, body),
        (Method::Post, ["api", "shipments", id, "deliver"]) => deliver(route, id),
        (Method::Post, ["api", "orders", id, "refund"]) => refund(route, id, body),
        (Method::Post, ["api", "orders", id, "returns"]) => request_return(route, id, body),
        (Method::Post, ["api", "returns", id, "approve"]) => decide_return(route, id, true),
        (Method::Post, ["api", "returns", id, "reject"]) => decide_return(route, id, false),
        _ => Reply::err(404, "not_found"),
    }
}

/// Ask the fsm engine itself whether a transition is legal — never a
/// hand-rolled comparison of a status string.
fn can_fire(machine: &str, instance: &str, event: &str) -> bool {
    matches!(fsm::can_fire(machine, instance, event), Ok(true))
}

/// The vendor the order's money settles to — read straight off the first
/// item's listing (the contract's ledger shape is one vendor account per
/// entry).
fn order_vendor(order: &Value) -> Option<String> {
    let items = order.get("items")?.as_array()?;
    let listing_id = items.first()?.get("listing_id")?.as_str()?;
    let entry = records::get("listings", listing_id).ok()?;
    let listing: Value = serde_json::from_str(&entry.data).ok()?;
    listing.get("vendor")?.as_str().map(str::to_string)
}

/// True when `subject` is the vendor of any listing in the order.
fn order_has_vendor(order: &Value, subject: &str) -> bool {
    let items = match order.get("items").and_then(Value::as_array) {
        Some(items) => items,
        None => return false,
    };
    for item in items {
        let listing_id = match item.get("listing_id").and_then(Value::as_str) {
            Some(id) => id,
            None => continue,
        };
        let entry = match records::get("listings", listing_id) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let listing: Value = match serde_json::from_str(&entry.data) {
            Ok(listing) => listing,
            Err(_) => continue,
        };
        if listing.get("vendor").and_then(Value::as_str) == Some(subject) {
            return true;
        }
    }
    false
}

fn pay(route: &Route, order_id: &str) -> Reply {
    let principal = authenticated!(route);
    let order_entry = match records::get("orders", order_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&order_entry.data).unwrap_or(json!({}));
    let buyer = order.get("buyer").and_then(Value::as_str).unwrap_or("");
    if principal.subject != buyer {
        audit("payment.pay", "deny", &principal.subject, order_id);
        return Reply::err(403, "forbidden");
    }
    if !can_fire("order", order_id, "pay") {
        return Reply::err(409, "illegal_transition");
    }

    if records::create(
        "payments",
        &json!({"order_id": order_id, "refunded_amount": 0}).to_string(),
        &["order_id".to_string()],
    )
    .is_err()
    {
        return Reply::err(500, "store_error");
    }
    if fsm::create_instance("payment", order_id).is_err()
        || fsm::fire("payment", order_id, "capture").is_err()
    {
        return Reply::err(500, "fsm_error");
    }
    if fsm::fire("order", order_id, "pay").is_err() {
        return Reply::err(409, "illegal_transition");
    }

    let total = order.get("total").and_then(Value::as_i64).unwrap_or(0);
    let fee = total / 10;
    let vendor_amount = total - fee;
    let vendor = match order_vendor(&order) {
        Some(vendor) => vendor,
        None => return Reply::err(500, "vendor_unknown"),
    };
    let vendor_account = format!("vendor:{vendor}");
    let memo = format!("order {order_id} sale");
    let entry = ledger::Entry {
        id: String::new(),
        memo: memo.clone(),
        lines: vec![
            ledger::Line {
                account: "platform:cash".to_string(),
                amount: total,
                side: ledger::Side::Debit,
            },
            ledger::Line {
                account: vendor_account.clone(),
                amount: vendor_amount,
                side: ledger::Side::Credit,
            },
            ledger::Line {
                account: "platform:fees".to_string(),
                amount: fee,
                side: ledger::Side::Credit,
            },
        ],
    };
    if ledger::validate(&entry).is_err() {
        return Reply::err(500, "invalid_ledger_entry");
    }
    let stored = json!({
        "memo": memo,
        "lines": [
            {"account": "platform:cash", "amount": total, "side": "debit"},
            {"account": vendor_account, "amount": vendor_amount, "side": "credit"},
            {"account": "platform:fees", "amount": fee, "side": "credit"},
        ],
    });
    if records::create("ledger_entries", &stored.to_string(), &[]).is_err() {
        return Reply::err(500, "store_error");
    }
    audit("payment.pay", "allow", &principal.subject, order_id);
    Reply::json(200, json!({"order_id": order_id, "status": "paid"}))
}

fn ship(route: &Route, order_id: &str, body: &str) -> Reply {
    let principal = authenticated!(route);
    let order_entry = match records::get("orders", order_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&order_entry.data).unwrap_or(json!({}));
    if !is_admin(&principal) && !order_has_vendor(&order, principal.subject.as_str()) {
        audit("order.ship", "deny", &principal.subject, order_id);
        return Reply::err(403, "forbidden");
    }
    if !can_fire("order", order_id, "ship") {
        return Reply::err(409, "illegal_transition");
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let carrier = req.get("carrier").and_then(Value::as_str).unwrap_or("").to_string();
    let tracking = req.get("tracking").and_then(Value::as_str).unwrap_or("").to_string();
    let shipment = match records::create(
        "shipments",
        &json!({"order_id": order_id, "carrier": carrier, "tracking": tracking}).to_string(),
        &["order_id".to_string()],
    ) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let shipment_id = shipment.id;
    if fsm::create_instance("shipment", &shipment_id).is_err() {
        return Reply::err(500, "fsm_error");
    }
    if fsm::fire("shipment", &shipment_id, "dispatch").is_err() {
        return Reply::err(500, "fsm_error");
    }
    if fsm::fire("order", order_id, "ship").is_err() {
        return Reply::err(409, "illegal_transition");
    }
    audit("order.ship", "allow", &principal.subject, order_id);
    Reply::json(200, json!({"id": shipment_id, "order_id": order_id, "status": "in_transit"}))
}

fn deliver(route: &Route, shipment_id: &str) -> Reply {
    let principal = authenticated!(route);
    if !is_admin(&principal) {
        audit("shipment.deliver", "deny", &principal.subject, shipment_id);
        return Reply::err(403, "forbidden");
    }
    let shipment_entry = match records::get("shipments", shipment_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let shipment: Value = serde_json::from_str(&shipment_entry.data).unwrap_or(json!({}));
    let order_id = shipment.get("order_id").and_then(Value::as_str).unwrap_or("").to_string();
    if order_id.is_empty() {
        return Reply::err(500, "shipment_missing_order");
    }
    // Cross-machine rule: the ORDER's `deliver` is fired (and confirmed)
    // FIRST, so a failure there never leaves the shipment half-applied —
    // `fsm:workflow` has no cross-machine transaction, so the shipment's own
    // transition goes SECOND.
    if fsm::fire("order", &order_id, "deliver").is_err() {
        return Reply::json(409, json!({"error": "illegal_transition", "machine": "order"}));
    }
    if fsm::fire("shipment", shipment_id, "deliver").is_err() {
        return Reply::json(409, json!({"error": "illegal_transition", "machine": "shipment"}));
    }
    audit("shipment.deliver", "allow", &principal.subject, shipment_id);
    Reply::json(200, json!({"id": shipment_id, "order_id": order_id, "status": "delivered"}))
}

fn refund(route: &Route, order_id: &str, body: &str) -> Reply {
    let principal = authenticated!(route);
    if !is_admin(&principal) {
        audit("payment.refund", "deny", &principal.subject, order_id);
        return Reply::err(403, "forbidden");
    }
    let order_entry = match records::get("orders", order_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&order_entry.data).unwrap_or(json!({}));
    let total = order.get("total").and_then(Value::as_i64).unwrap_or(0);
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let amount = match req.get("amount").and_then(Value::as_i64) {
        Some(amount) if amount > 0 => amount,
        _ => return Reply::err(400, "invalid_amount"),
    };
    let order_id_json = serde_json::to_string(&order_id.to_string()).unwrap_or_default();
    let payment_entry = match records::find_by("payments", "order_id", &order_id_json) {
        Ok(entries) if !entries.is_empty() => entries.into_iter().next().unwrap(),
        _ => return Reply::err(409, "no_payment"),
    };
    let mut payment: Value = serde_json::from_str(&payment_entry.data).unwrap_or(json!({}));
    let refunded = payment.get("refunded_amount").and_then(Value::as_i64).unwrap_or(0);
    if refunded + amount > total {
        return Reply::err(400, "refund_exceeds_total");
    }
    payment["refunded_amount"] = json!(refunded + amount);
    if records::update("payments", &payment_entry.id, &payment.to_string(), payment_entry.revision)
        .is_err()
    {
        return Reply::err(500, "store_error");
    }
    // The machine only tracks THAT a refund happened; a later partial refund
    // against an already-`refunded` instance just updates the amount.
    if can_fire("payment", order_id, "refund") {
        let _ = fsm::fire("payment", order_id, "refund");
    }

    let vendor = match order_vendor(&order) {
        Some(vendor) => vendor,
        None => return Reply::err(500, "vendor_unknown"),
    };
    let vendor_account = format!("vendor:{vendor}");
    let memo = format!("order {order_id} refund");
    let entry = ledger::Entry {
        id: String::new(),
        memo: memo.clone(),
        lines: vec![
            ledger::Line {
                account: vendor_account.clone(),
                amount,
                side: ledger::Side::Debit,
            },
            ledger::Line {
                account: "platform:cash".to_string(),
                amount,
                side: ledger::Side::Credit,
            },
        ],
    };
    if ledger::validate(&entry).is_err() {
        return Reply::err(500, "invalid_ledger_entry");
    }
    let stored = json!({
        "memo": memo,
        "lines": [
            {"account": vendor_account, "amount": amount, "side": "debit"},
            {"account": "platform:cash", "amount": amount, "side": "credit"},
        ],
    });
    if records::create("ledger_entries", &stored.to_string(), &[]).is_err() {
        return Reply::err(500, "store_error");
    }
    audit("payment.refund", "allow", &principal.subject, order_id);
    Reply::json(200, json!({"order_id": order_id, "refunded_amount": refunded + amount}))
}

fn request_return(route: &Route, order_id: &str, body: &str) -> Reply {
    let principal = authenticated!(route);
    let order_entry = match records::get("orders", order_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&order_entry.data).unwrap_or(json!({}));
    let buyer = order.get("buyer").and_then(Value::as_str).unwrap_or("");
    if principal.subject != buyer {
        audit("return.request", "deny", &principal.subject, order_id);
        return Reply::err(403, "forbidden");
    }
    let delivered = matches!(
        fsm::get_status("order", order_id),
        Ok(status) if status.state == "delivered"
    );
    if !delivered {
        return Reply::err(409, "illegal_transition");
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let reason = req.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
    let created = match records::create(
        "returns",
        &json!({"order_id": order_id, "buyer": principal.subject, "reason": reason}).to_string(),
        &["order_id".to_string()],
    ) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let return_id = created.id;
    if fsm::create_instance("return", &return_id).is_err() {
        return Reply::err(500, "fsm_error");
    }
    audit("return.request", "allow", &principal.subject, &return_id);
    Reply::json(201, json!({"id": return_id, "order_id": order_id, "status": "requested"}))
}

fn decide_return(route: &Route, return_id: &str, approve: bool) -> Reply {
    let principal = authenticated!(route);
    let event_name = if approve { "return.approve" } else { "return.reject" };
    if !is_admin(&principal) {
        audit(event_name, "deny", &principal.subject, return_id);
        return Reply::err(403, "forbidden");
    }
    if records::get("returns", return_id).is_err() {
        return Reply::err(404, "not_found");
    }
    let (event, state) = if approve { ("approve", "approved") } else { ("reject", "rejected") };
    if fsm::fire("return", return_id, event).is_err() {
        return Reply::err(409, "illegal_transition");
    }
    audit(event_name, "allow", &principal.subject, return_id);
    Reply::json(200, json!({"id": return_id, "status": state}))
}