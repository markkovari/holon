//! `swaps` — a ticket changes hands, and the house is no fuller than it was.
//!
//! The rule that decides this part is a negative: accepting a swap touches
//! `quota:meter` NOT AT ALL. A swap moves a ticket between holders; it is not a
//! release followed by a claim, and modelling it that way opens the released place
//! to the public for as long as the gap lasts, while looking correct to any test
//! that only asks who holds what afterwards.

use serde_json::json;

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::store::{find_by_str, load, save, with_id};
use crate::{require, Reply, Route};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "swaps"]) => offer(route, body),
        (Method::Get, ["api", "swaps"]) => open_offers(route),
        (Method::Post, ["api", "swaps", id, "accept"]) => accept(route, id),
        (Method::Delete, ["api", "swaps", id]) => withdraw(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn offer(route: &Route, body: &str) -> Reply {
    let p = match require(route, "swap", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Ok(input) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "malformed_body");
    };
    let ticket_id = input["ticket_id"].as_str().unwrap_or_default().to_string();
    let (_, ticket) = match load("tickets", &ticket_id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if ticket["holder"].as_str() != Some(p.subject.as_str()) {
        return Reply::err(403, "forbidden");
    }
    // A checked-in ticket has been used and a released one is gone; neither is a
    // thing to hand on.
    if ticket["state"].as_str() != Some("issued") {
        return Reply::err(409, "not_swappable");
    }
    let open = find_by_str("swaps", "ticket_id", &ticket_id).into_iter().any(|e| {
        let d: serde_json::Value = serde_json::from_str(&e.data).unwrap_or_default();
        d["state"].as_str() == Some("offered")
    });
    if open {
        return Reply::err(409, "already_offered");
    }

    let doc = json!({
        "ticket_id": ticket_id,
        "from": p.subject,
        "to": serde_json::Value::Null,
        "state": "offered",
        "created_at": ticket["issued_at"],
    });
    match records::create(
        "swaps",
        &doc.to_string(),
        &["ticket_id".to_string(), "state".to_string()],
    ) {
        Ok(e) => Reply::json(201, with_id(&e)),
        Err(_) => Reply::err(500, "store_failed"),
    }
}

fn open_offers(route: &Route) -> Reply {
    if let Err(r) = require(route, "swap", "write") {
        return r;
    }
    let swaps: Vec<_> = find_by_str("swaps", "state", "offered").iter().map(with_id).collect();
    Reply::json(200, json!({ "swaps": swaps }))
}

fn accept(route: &Route, id: &str) -> Reply {
    let p = match require(route, "swap", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (swap_entry, mut swap) = match load("swaps", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if swap["state"].as_str() != Some("offered") {
        return Reply::err(409, "not_offered");
    }
    if swap["from"].as_str() == Some(p.subject.as_str()) {
        return Reply::err(403, "cannot_accept_own");
    }

    let ticket_id = swap["ticket_id"].as_str().unwrap_or_default().to_string();
    let (ticket_entry, mut ticket) = match load("tickets", &ticket_id) {
        Ok(v) => v,
        Err(r) => return r,
    };

    // The ticket moves. The meter does not — see this module's header.
    ticket["holder"] = json!(p.subject);
    if let Err(r) = save("tickets", &ticket_entry, &ticket) {
        return r;
    }
    swap["state"] = json!("accepted");
    swap["to"] = json!(p.subject);
    if let Err(r) = save("swaps", &swap_entry, &swap) {
        return r;
    }
    let mut out = swap;
    out["id"] = json!(swap_entry.id);
    Reply::json(200, out)
}

fn withdraw(route: &Route, id: &str) -> Reply {
    let p = match require(route, "swap", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (entry, mut doc) = match load("swaps", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if doc["from"].as_str() != Some(p.subject.as_str()) {
        return Reply::err(403, "forbidden");
    }
    if doc["state"].as_str() != Some("offered") {
        return Reply::err(409, "not_offered");
    }
    doc["state"] = json!("withdrawn");
    if let Err(r) = save("swaps", &entry, &doc) {
        return r;
    }
    Reply::no_content()
}
