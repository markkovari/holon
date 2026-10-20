//! The admin's own verdict is recorded; the `jev_*` fields never decide anything.
//! Reputation reads orders/listings/disputes straight from records:store (parts never call each other).

use std::collections::HashSet;

use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::jev::decision::decision as jev;
use crate::bindings::p3::http::types::Method;
use crate::bindings::records::store::store as records;
use crate::{is_admin, Reply, Route};
use serde_json::{json, Value};

/// Verbatim from CONTRACT.md.
const TRUE_HINT: &str = "the buyer's claim looks legitimate and a refund is warranted";
const FALSE_HINT: &str = "the claim looks weak or abusive";

const RESOLUTIONS: &[&str] = &["refund", "deny", "partial"];

/// A "large order" for fraud scoring: 100 dollars, in minor units.
const LARGE_ORDER: u64 = 10_000;

const REFUND_THRESHOLD: u32 = 500;

/// Not `guestauth::guest_authenticated!`: its expansion names `introspect` in THIS module's scope.
macro_rules! principal_of {
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
        (Method::Post, ["api", "orders", order_id, "disputes"]) => {
            open_dispute(route, order_id, body)
        }
        (Method::Get, ["api", "disputes"]) => list_disputes(route),
        (Method::Post, ["api", "disputes", id, "resolve"]) => resolve_dispute(route, id, body),
        (Method::Get, ["api", "vendors", subject, "reputation"]) => reputation(route, subject),
        _ => Reply::err(404, "not_found"),
    }
}

/// Idempotent; this part owns `dispute`, and no other part may have run its startup path.
fn ensure_machines() {
    let _ = fsm::define(
        "dispute",
        &fsm::Definition {
            states: vec!["open".to_string(), "resolved".to_string()],
            initial: "open".to_string(),
            transitions: vec![fsm::Transition {
                event: "resolve".to_string(),
                source: "open".to_string(),
                target: "resolved".to_string(),
            }],
            terminal: vec!["resolved".to_string()],
        },
    );
}

/// A read error means "nothing there": these aggregate over another part's collection.
fn load_all(collection: &str) -> Vec<(String, Value)> {
    match crate::list_all(collection) {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| {
                let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
                (entry.id, data)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn as_int(value: &Value) -> u64 {
    if let Some(n) = value.as_u64() {
        n
    } else if let Some(f) = value.as_f64() {
        if f.is_finite() && f > 0.0 {
            f as u64
        } else {
            0
        }
    } else if let Some(s) = value.as_str() {
        s.parse::<u64>().unwrap_or(0)
    } else {
        0
    }
}

/// The Jev gate is advisory and must never refuse creation.
fn open_dispute(route: &Route, order_id: &str, body: &str) -> Reply {
    let principal = principal_of!(route);
    let subject: &str = principal.subject.as_ref();

    let order_entry = match records::get("orders", order_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&order_entry.data).unwrap_or(Value::Null);
    let buyer = order.get("buyer").and_then(Value::as_str).unwrap_or("").to_string();
    if buyer != subject {
        return Reply::err(403, "forbidden");
    }

    ensure_machines();

    // An instance that does not exist cannot be `delivered`, so a lookup error does not block creation.
    if let Ok(status) = fsm::get_status("order", order_id) {
        if status.state == "delivered" {
            return Reply::err(409, "illegal_transition");
        }
    }

    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let reason = parsed.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
    let total = order.get("total").map(as_int).unwrap_or(0);

    // ADVISORY ONLY: on any failure the dispute is still created with the `jev_*` fields empty for an admin to resolve.
    let mut jev_recommendation = String::new();
    let mut jev_confidence: u32 = 0;
    let gate = jev::GateRequest {
        state: format!("{reason} (order total: {total})"),
        instructions: "Judge whether this buyer's dispute looks legitimate.".to_string(),
        true_hint: TRUE_HINT.to_string(),
        false_hint: FALSE_HINT.to_string(),
    };
    if let Ok(result) = jev::gate(&gate) {
        jev_confidence = result.probability.min(1000);
        jev_recommendation = if result.probability >= REFUND_THRESHOLD {
            "refund".to_string()
        } else {
            "deny".to_string()
        };
    }

    let document = json!({
        "order_id": order_id,
        "buyer": buyer,
        "reason": reason,
        "resolution": "",
        "jev_recommendation": jev_recommendation,
        "jev_confidence": jev_confidence,
    });
    let entry = match records::create("disputes", &document.to_string(), &["order_id".to_string()])
    {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };
    if fsm::create_instance("dispute", &entry.id).is_err() {
        return Reply::err(500, "store_error");
    }

    let mut out = document;
    out["id"] = json!(entry.id);
    out["status"] = json!("open");
    Reply::json(201, out)
}

fn list_disputes(route: &Route) -> Reply {
    let principal = principal_of!(route);
    let subject: &str = principal.subject.as_ref();
    let admin = is_admin(&principal);

    let mut disputes = Vec::new();
    for (id, data) in load_all("disputes") {
        let mut document = data;
        let is_buyer = document.get("buyer").and_then(Value::as_str) == Some(subject);
        if !admin && !is_buyer {
            continue;
        }
        if let Value::Object(ref mut map) = document {
            map.insert("id".to_string(), json!(id));
            if let Ok(status) = fsm::get_status("dispute", &id) {
                map.insert("status".to_string(), json!(status.state));
            }
        }
        disputes.push(document);
    }
    Reply::json(200, json!({"disputes": disputes}))
}

/// Never reads the `jev_*` fields to decide, and never moves money.
fn resolve_dispute(route: &Route, id: &str, body: &str) -> Reply {
    let principal = principal_of!(route);
    if !is_admin(&principal) {
        return Reply::err(403, "forbidden");
    }

    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let resolution = parsed.get("resolution").and_then(Value::as_str).unwrap_or("").to_string();
    if !RESOLUTIONS.contains(&resolution.as_str()) {
        return Reply::err(400, "invalid_resolution");
    }

    let entry = match records::get("disputes", id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };

    ensure_machines();
    if fsm::fire("dispute", id, "resolve").is_err() {
        return Reply::err(409, "illegal_transition");
    }

    let mut document: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    if !document.is_object() {
        document = json!({});
    }
    document["resolution"] = json!(resolution.clone());
    match records::update("disputes", id, &document.to_string(), entry.revision) {
        Ok(_) => {
            Reply::json(200, json!({"id": id, "resolution": resolution, "status": "resolved"}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

/// `fraud_flags` (CONTRACT.md): an order over 100 dollars that is also the vendor's FIRST-ever order.
fn reputation(route: &Route, subject: &str) -> Reply {
    let _principal = principal_of!(route);

    let mut vendor_listings: HashSet<String> = HashSet::new();
    for (id, data) in load_all("listings") {
        if data.get("vendor").and_then(Value::as_str) == Some(subject) {
            vendor_listings.insert(id);
        }
    }

    // Oldest first: the fraud rule is about the FIRST-ever order.
    let mut orders: Vec<_> = Vec::new();
    if let Ok(entries) = crate::list_all("orders") {
        for entry in entries {
            let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
            let touches_vendor = data
                .get("items")
                .and_then(Value::as_array)
                .map(|items| {
                    items.iter().any(|item| {
                        item.get("listing_id")
                            .and_then(Value::as_str)
                            .map(|listing_id| vendor_listings.contains(listing_id))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if touches_vendor {
                orders.push((entry.created, entry.id, data));
            }
        }
    }
    orders.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let order_count = orders.len();
    let order_ids: HashSet<&str> = orders.iter().map(|(_, id, _)| id.as_str()).collect();

    let mut fraud_flags = 0u64;
    for (earlier_orders, (_, _, order)) in orders.iter().enumerate() {
        let total = order.get("total").map(as_int).unwrap_or(0);
        if earlier_orders == 0 && total > LARGE_ORDER {
            fraud_flags += 1;
        }
    }

    let mut dispute_count = 0u64;
    for (_, data) in load_all("disputes") {
        if let Some(order_id) = data.get("order_id").and_then(Value::as_str) {
            if order_ids.contains(order_id) {
                dispute_count += 1;
            }
        }
    }

    let dispute_rate =
        if order_count == 0 { 0.0 } else { dispute_count as f64 / order_count as f64 };

    Reply::json(
        200,
        json!({
            "vendor": subject,
            "dispute_rate": dispute_rate,
            "order_count": order_count,
            "fraud_flags": fraud_flags,
        }),
    )
}
