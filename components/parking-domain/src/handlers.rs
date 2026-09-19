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
use std::collections::{BTreeMap, HashSet};

const POLICY_DOMAIN: &str = "reservations";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "subject");
guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "spots"]) => create_spot(route, body),
        (Method::Get, ["api", "spots"]) => list_spots(route),
        (Method::Post, ["api", "spots", id, "reservations"]) => reserve(route, id, body),
        (Method::Get, ["api", "reservations"]) => list_reservations(route),
        (Method::Post, ["api", "reservations", id, "cancel"]) => cancel(route, id),
        (Method::Post, ["api", "lot", "seed"]) => seed_lot(route),
        (Method::Get, ["api", "lot"]) => get_lot(route),
        (Method::Get, ["api", "spots", id, "history"]) => spot_history(route, id),
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
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "spot.create", "");
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
    let principal = guestauth::guest_authenticated!(route);
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
    let principal = guestauth::guest_authenticated!(route);
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
    let principal = guestauth::guest_authenticated!(route);
    let entry = match records::get("reservations", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut res: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let subject = res.get("subject").and_then(Value::as_str).unwrap_or("").to_string();
    guestauth::guest_deny_unless!(owns_or_admin("cancel", &principal, &subject), principal, "reservation.cancel", id);
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

/// A lot-seeded spot carries BOTH `floor` and `number`; an ad-hoc spot made
/// through `POST /api/spots` carries neither. That difference is the only
/// thing telling the two populations apart in the shared `spots` collection.
fn is_lot_spot(data: &str) -> bool {
    let v: Value = serde_json::from_str(data).unwrap_or(json!({}));
    v.get("floor").and_then(Value::as_u64).is_some()
        && v.get("number").and_then(Value::as_u64).is_some()
}

/// `admin`-only and idempotent: seed exactly 150 lot spots (3 floors × 50,
/// `"F1-01"`..`"F3-50"`). A marker record in `meta` — the same trick
/// `guest_owner_or_admin_policy!` uses for its rule registration — makes a
/// second call create NOTHING more.
fn seed_lot(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "lot.seed", "");
    let already = matches!(
        records::find_by("meta", "kind", "\"lot_seeded\""),
        Ok(entries) if !entries.is_empty()
    );
    if !already {
        for floor in 1..=3u32 {
            for number in 1..=50u32 {
                let data = json!({
                    "label": format!("F{}-{:02}", floor, number),
                    "floor": floor,
                    "number": number,
                })
                .to_string();
                if records::create("spots", &data, &[]).is_err() {
                    return Reply::err(500, "store_error");
                }
            }
        }
        let marker = json!({"kind": "lot_seeded"}).to_string();
        let _ = records::create("meta", &marker, &["kind".to_string()]);
    }
    let total = match list_all("spots") {
        Ok(entries) => entries.iter().filter(|e| is_lot_spot(&e.data)).count(),
        Err(_) => return Reply::err(500, "store_error"),
    };
    audit("lot.seed", "allow", &principal.subject, "");
    Reply::json(200, json!({"total": total}))
}

/// Any authenticated caller: every lot-seeded spot, grouped by floor. `taken`
/// is computed live on every call — never stored — from active reservations
/// whose HALF-OPEN window covers `now`: `start <= now < end`, the same
/// comparison the overlap rule above uses. An unseeded lot is `{"floors": []}`.
fn get_lot(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    let entries = match list_all("spots") {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let reservations = match list_all("reservations") {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let now = crate::now_secs();
    let mut taken: HashSet<String> = HashSet::new();
    for e in &reservations {
        let v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
        if v.get("status").and_then(Value::as_str) != Some("active") {
            continue;
        }
        let start = v.get("start").and_then(Value::as_u64).unwrap_or(0);
        let end = v.get("end").and_then(Value::as_u64).unwrap_or(0);
        if start <= now && now < end {
            if let Some(sid) = v.get("spot_id").and_then(Value::as_str) {
                taken.insert(sid.to_string());
            }
        }
    }
    let mut by_floor: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    for e in &entries {
        let v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
        let floor = match v.get("floor").and_then(Value::as_u64) {
            Some(f) => f,
            None => continue,
        };
        let number = match v.get("number").and_then(Value::as_u64) {
            Some(n) => n,
            None => continue,
        };
        by_floor.entry(floor).or_default().push(json!({
            "id": e.id,
            "number": number,
            "taken": taken.contains(&e.id),
        }));
    }
    let floors: Vec<Value> = by_floor
        .into_iter()
        .map(|(floor, mut spots)| {
            spots.sort_by_key(|s| s.get("number").and_then(Value::as_u64).unwrap_or(0));
            json!({"floor": floor, "spots": spots})
        })
        .collect();
    Reply::json(200, json!({"floors": floors}))
}

/// `admin`-only: every reservation ever made on a spot — active AND cancelled
/// — oldest first, whether the spot is lot-seeded or ad-hoc. A spot with no
/// reservations answers `[]`, not 404; only a spot that does not exist is 404.
fn spot_history(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "spot.history", id);
    if records::get("spots", id).is_err() {
        return Reply::err(404, "not_found");
    }
    let spot_json = serde_json::to_string(id).unwrap_or_default();
    let entries = match records::find_by("reservations", "spot_id", &spot_json) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let mut reservations = entries_json(&entries);
    reservations.sort_by_key(|r| r.get("start").and_then(Value::as_u64).unwrap_or(0));
    Reply::json(200, json!({"spot_id": id, "reservations": reservations}))
}