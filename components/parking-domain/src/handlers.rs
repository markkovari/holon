//! Spots + reservations. A reservation's window is HALF-OPEN `[start, end)`:
//! two windows on the same spot conflict only when `a.start < b.end && b.start
//! < a.end`, so adjacent bookings (one ending exactly when the next starts)
//! are both allowed. Overlap is a rule `auth:identity/rbac` cannot express at
//! all — no role decides whether two time ranges intersect — so the check is
//! plain Rust over the `records::find_by("reservations", "spot_id", …)`
//! result. Cancelling IS a row-level rule (owner or admin), enforced with
//! `policy:guard` through `guestauth::guest_owner_or_admin_policy!`.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const POLICY_DOMAIN: &str = "reservations";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "subject");

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "spots"]) => create_spot(route, body),
        (Method::Get, ["api", "spots"]) => list_spots(route),
        (Method::Post, ["api", "spots", id, "reservations"]) => reserve(route, id, body),
        (Method::Get, ["api", "reservations"]) => list_reservations(route),
        (Method::Post, ["api", "reservations", id, "cancel"]) => cancel(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

#[derive(serde::Deserialize)]
struct SpotReq {
    #[serde(default)]
    label: String,
}

/// `admin`-only: a role, not a row, decides who may open a new spot.
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
    let label = req.label.trim().to_string();
    if label.is_empty() {
        return Reply::err(400, "label is required");
    }
    let data = json!({"label": label}).to_string();
    match records::create("spots", &data, &[]) {
        Ok(entry) => {
            audit("spot.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id, "label": label}))
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
struct ReservationReq {
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
    let req: ReservationReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    // Half-open windows must have positive length; checked BEFORE conflicts.
    if req.end <= req.start {
        return Reply::err(400, "end must be after start");
    }
    if records::get("spots", spot_id).is_err() {
        return Reply::err(404, "not_found");
    }
    let spot_json = serde_json::to_string(spot_id).unwrap_or_default();
    let existing = match records::find_by("reservations", "spot_id", &spot_json) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    for entry in &existing {
        let v: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        if v.get("status").and_then(Value::as_str) != Some("active") {
            continue;
        }
        let s = v.get("start").and_then(Value::as_u64).unwrap_or(0);
        let e = v.get("end").and_then(Value::as_u64).unwrap_or(0);
        // `[req.start, req.end)` overlaps `[s, e)` only here. Touching at a
        // boundary is not an overlap: neither comparison is `<=`.
        if req.start < e && s < req.end {
            audit("reservation.create", "deny", &principal.subject, spot_id);
            return Reply::err(409, "overlap");
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
            Reply::json(201, json!({"id": entry.id, "status": "active"}))
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
        records::find_by("reservations", "subject", &subject_json).map_err(|_| ())
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
    let subject = res.get("subject").and_then(Value::as_str).unwrap_or("").to_string();
    if !owns_or_admin("cancel", &principal, &subject) {
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

/// `list_records` is cursor-paginated — page until it comes back short, or a
/// listing silently truncates at the page size.
fn list_all(collection: &str) -> Result<Vec<records::Entry>, ()> {
    let mut all: Vec<records::Entry> = Vec::new();
    let mut after = String::new();
    loop {
        let page = records::list_records(collection, 100, &after).map_err(|_| ())?;
        let n = page.entries.len();
        all.extend(page.entries);
        if n < 100 || page.next.is_empty() || page.next == after {
            break;
        }
        after = page.next;
    }
    Ok(all)
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