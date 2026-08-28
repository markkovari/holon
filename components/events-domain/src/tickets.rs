//! `tickets` — claiming a free place, and the QR that proves it.
//!
//! The whole part turns on one line. `quota::reserve` is atomic; reading a count
//! and comparing it to a capacity is not, and the difference only shows when two
//! people claim the last place in the same instant — which is the one moment a
//! ticketing system exists to get right. Every sequential test passes either way.
//!
//! The QR carries the `code` and nothing else. `id:generate/nanoid` makes it
//! unguessable, so possession of the code IS the claim, and the door verifies it
//! against the store rather than against a signature.

use serde_json::json;

use crate::bindings::id::generate::generator as ids;
use crate::bindings::qr::encode::encoder as qr;
use crate::bindings::quota::meter::meter as quota;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::store::{find_by_str, load, quota_subject, with_id, QUOTA_PERIOD};
use crate::{has_role, require, Reply, Route};

/// A ticket that still occupies a place. A released one does not.
fn is_live(state: &str) -> bool {
    state == "issued" || state == "checked-in"
}

/// The scannable code, as an SVG document.
///
/// A QR that cannot be rendered is not a reason to refuse the ticket — the claim
/// succeeded and the code is in the response either way — so this degrades to an
/// empty string rather than a 500.
fn render(code: &str) -> String {
    qr::svg(code, qr::Ecc::Medium, 2).unwrap_or_default()
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "events", id, "tickets"]) => claim(route, id),
        (Method::Get, ["api", "tickets"]) => mine(route),
        (Method::Get, ["api", "tickets", id]) => one(route, id),
        (Method::Delete, ["api", "tickets", id]) => release(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn claim(route: &Route, event_id: &str) -> Reply {
    let p = match require(route, "ticket", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (_, event) = match load("events", event_id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if event["state"].as_str() != Some("open") {
        return Reply::err(409, "event_cancelled");
    }

    // One live ticket per person per event, checked BEFORE the reservation — a
    // refusal that had already taken a place would shrink the event by one every
    // time somebody clicked twice.
    let already = find_by_str("tickets", "event_id", event_id).into_iter().any(|e| {
        let d: serde_json::Value = serde_json::from_str(&e.data).unwrap_or_default();
        d["holder"].as_str() == Some(p.subject.as_str())
            && is_live(d["state"].as_str().unwrap_or_default())
    });
    if already {
        return Reply::err(409, "already_holding");
    }

    // THE line. Atomic: two claims for one place produce one success and one
    // `exceeded`, where count-then-create produces two tickets.
    let capacity = event["capacity"].as_u64().unwrap_or(0);
    if quota::reserve(&quota_subject(event_id), 1, capacity, QUOTA_PERIOD).is_err() {
        return Reply::err(409, "sold_out");
    }

    let code = ids::nanoid(21);
    let doc = json!({
        "event_id": event_id,
        "holder": p.subject,
        "code": code,
        "state": "issued",
        "issued_at": event["starts_at"],
        "checked_in_at": serde_json::Value::Null,
    });
    match records::create(
        "tickets",
        &doc.to_string(),
        &["event_id".to_string(), "holder".to_string(), "code".to_string()],
    ) {
        Ok(e) => {
            let mut out = with_id(&e);
            out["qr"] = json!(render(&code));
            Reply::json(201, out)
        }
        // The place is reserved and the ticket is not written. Left that way on
        // purpose: `reserve` has no matching release that cannot also free somebody
        // else's place, and one seat lost to a store failure is a smaller wrong than
        // a door that admits more people than the room holds.
        Err(_) => Reply::err(500, "store_failed"),
    }
}

fn mine(route: &Route) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let tickets: Vec<_> =
        find_by_str("tickets", "holder", &p.subject).iter().map(with_id).collect();
    Reply::json(200, json!({ "tickets": tickets }))
}

/// The holder, the organizer of that ticket's event, or an admin.
fn one(route: &Route, id: &str) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (entry, doc) = match load("tickets", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let is_holder = doc["holder"].as_str() == Some(p.subject.as_str());
    let is_organizer = match load("events", doc["event_id"].as_str().unwrap_or_default()) {
        Ok((_, ev)) => ev["organizer"].as_str() == Some(p.subject.as_str()),
        Err(_) => false,
    };
    if !(is_holder || is_organizer || has_role(&p, "admin")) {
        return Reply::err(403, "forbidden");
    }
    let mut out = with_id(&entry);
    out["qr"] = json!(render(doc["code"].as_str().unwrap_or_default()));
    Reply::json(200, out)
}

fn release(route: &Route, id: &str) -> Reply {
    let p = match require(route, "ticket", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (entry, mut doc) = match load("tickets", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if doc["holder"].as_str() != Some(p.subject.as_str()) {
        return Reply::err(403, "forbidden");
    }
    match crate::checkin::fire(&entry.id, "release") {
        Ok(status) => {
            doc["state"] = json!(status.state);
            if let Err(r) = crate::store::save("tickets", &entry, &doc) {
                return r;
            }
            Reply::no_content()
        }
        Err(state) => Reply::json(409, json!({ "error": "not_releasable", "state": state })),
    }
}
