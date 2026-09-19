//! Spots + reservations. A `member` may reserve any spot but sees only their
//! own reservations — the row-level rule `auth:identity/rbac` cannot express,
//! enforced with `policy:guard` via `guestauth::guest_owner_or_admin_policy!`
//! exactly as `billing-domain` does for invoices. Creating a spot is
//! `admin`-only, checked directly against `principal.roles`.
//!
//! A reservation's window is HALF-OPEN `[start, end)`, so two reservations on
//! the same spot conflict only when `a.start < b.end && b.start < a.end`;
//! touching at a boundary is allowed.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "parking-reservations";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "subject");

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "spots"]) => create_spot(route, body),
        (Method::Get, ["api", "spots"]) => list_spots(route),
        (Method::Post, ["api", "spots", spot_id, "reservations"]) => {
            reserve(route, spot_id, body)
        }
        (Method::Get, ["api", "reservations"]) => list_reservations(route),
        (Method::Post, ["api", "reservations", id, "cancel"]) => cancel(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

/// `records::list-records` is cursor-paginated; walk every page rather than
/// silently truncating at the first one.
fn list_all(collection: &str) -> Result<Vec<records::Entry>, records::StoreError> {
    let mut out: Vec<records::Entry> = Vec::new();
    let mut after = String::new();
    loop {
        let page = records::list_records(collection, 100, &after)?;
        let n = page.entries.len();
        out.extend(page.entries);
        if n < 100 || page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    Ok(out)
}

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

#[derive(serde::Deserialize)]
struct SpotReq {
    #[serde(default)]
    label: String,
}

/// Admin-only, checked directly against the role — anyone can *see* a spot,
/// only an admin may add one.
fn create_spot(route: &Route, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !is_admin(&principal) {
        audit("spot.create", "deny", &principal.subject, "");
        return Reply::err(403, "forbidden");
    }
    let req: SpotReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.label.is_empty() {
        return Reply::err(400, "label is required");
    }
    let data = json!({"label": req.label}).to_string();
    match records::create("spots", &data, &[]) {
        Ok(entry) => {
            audit("spot.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id, "label": req.label}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_spots(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    match list_all("spots") {
        Ok(entries) => Reply::json(200, json!({"spots": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct ReserveReq {
    #[serde(default)]
    start: u64,
    #[serde(default)]
    end: u64,
}

fn reserve(route: &Route, spot_id: &str, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: ReserveReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    // The window check comes FIRST — before the spot lookup and before any
    // conflict scan.
    if req.end <= req.start {
        return Reply::err(400, "end must be greater than start");
    }
    if records::get("spots", spot_id).is_err() {
        return Reply::err(404, "not_found");
    }

    let spot_json = serde_json::to_string(spot_id).unwrap_or_default();
    let existing = match records::find_by("reservations", "spot_id", &spot_json) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    for entry in existing {
        let r: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        if r.get("status").and_then(Value::as_str) != Some("active") {
            continue;
        }
        let a_start = r.get("start").and_then(Value::as_u64).unwrap_or(0);
        let a_end = r.get("end").and_then(Value::as_u64).unwrap_or(0);
        // Half-open [start, end): touching boundaries do NOT overlap.
        if req.start < a_end && a_start < req.end {
            audit("reservation.create", "deny", &principal.subject, spot_id);
            return Reply::err(409, "conflict");
        }
    }

    let data = json!({
        "spot_id": spot_id,
        "subject": principal.subject,
        "start": req.start,
        "end": req.end,
        "status": "active",
    })
    .to_string();
    match records::create(
        "reservations",
        &data,
        &["spot_id".to_string(), "subject".to_string()],
    ) {
        Ok(entry) => {
            audit("reservation.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_reservations(route: &Route) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = if is_admin(&principal) {
        list_all("reservations")
    } else {
        let subject_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        records::find_by("reservations", "subject", &subject_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"reservations": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn cancel(route: &Route, id: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let entry = match records::get("reservations", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut res: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let owner = res.get("subject").and_then(Value::as_str).unwrap_or("").to_string();
    if !owns_or_admin("cancel", &principal, &owner) {
        audit("reservation.cancel", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }
    if res.get("status").and_then(Value::as_str) == Some("cancelled") {
        return Reply::err(400, "already cancelled");
    }
    res["status"] = json!("cancelled");
    match records::update("reservations", id, &res.to_string(), entry.revision) {
        Ok(_) => {
            audit("reservation.cancel", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "status": "cancelled"}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}