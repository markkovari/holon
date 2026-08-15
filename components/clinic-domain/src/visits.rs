//! Visits booked against a vet's day. **This file is the goal of the `visits` part.**
//!
//! `CONTRACT.md` is the specification — including the one rule a compiler cannot
//! check for you: a vet cannot be double-booked, and touching at the boundary is
//! not an overlap.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    // route.segments starts with ["api", "visits", ...]
    let rest = &route.segments[2.min(route.segments.len())..];
    match (method, rest) {
        (Method::Post, []) => create_visit(body),
        (Method::Get, []) => list_visits(route),
        (Method::Get, [id]) => get_visit(id),
        (Method::Delete, [id]) => delete_visit(id),
        _ => Reply::err(404, "not_found"),
    }
}

fn create_visit(body: &str) -> Reply {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "invalid"),
    };
    let pet_id = match v.get("pet_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Reply::err(400, "invalid"),
    };
    let vet = match v.get("vet").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Reply::err(400, "invalid"),
    };
    let start = match v.get("start").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Reply::err(400, "invalid"),
    };
    let minutes = match v.get("minutes").and_then(Value::as_u64) {
        Some(m) => m as u32,
        None => return Reply::err(400, "invalid"),
    };
    if minutes != 15 && minutes != 30 && minutes != 60 {
        return Reply::err(400, "invalid");
    }

    let start_ts = match parse_rfc3339(&start) {
        Some(t) => t,
        None => return Reply::err(400, "invalid"),
    };
    let end_ts = start_ts + (minutes as i64) * 60;

    match records::get("pets", &pet_id) {
        Ok(_) => {}
        Err(records::StoreError::NotFound) => return Reply::err(404, "not_found"),
        Err(_) => return Reply::err(500, "store_error"),
    }

    let existing = match records::list_records("visits", 10_000, "") {
        Ok(p) => p.entries,
        Err(_) => return Reply::err(500, "store_error"),
    };
    for e in &existing {
        let doc: Value = match serde_json::from_str(&e.data) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if doc.get("vet").and_then(Value::as_str) != Some(vet.as_str()) {
            continue;
        }
        let e_start = match doc.get("start").and_then(Value::as_str).and_then(parse_rfc3339) {
            Some(t) => t,
            None => continue,
        };
        let e_minutes = doc.get("minutes").and_then(Value::as_u64).unwrap_or(0) as i64;
        let e_end = e_start + e_minutes * 60;
        // Overlap iff the intervals genuinely intersect; touching at the
        // boundary (end == start of the other) is NOT an overlap.
        if start_ts < e_end && e_start < end_ts {
            return Reply::json(409, json!({"error": "clash", "with": e.id}));
        }
    }

    let doc = json!({"pet_id": pet_id, "vet": vet, "start": start, "minutes": minutes, "deleted": false});
    match records::create("visits", &doc.to_string(), &[]) {
        Ok(entry) => Reply::json(
            201,
            json!({"id": entry.id, "pet_id": pet_id, "vet": vet, "start": start, "minutes": minutes}),
        ),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_visits(route: &Route) -> Reply {
    let vet = route.param("vet");
    let day = route.param("day");
    let entries = match records::list_records("visits", 10_000, "") {
        Ok(p) => p.entries,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let mut visits: Vec<Value> = Vec::new();
    for e in &entries {
        let doc: Value = match serde_json::from_str(&e.data) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if !vet.is_empty() && doc.get("vet").and_then(Value::as_str) != Some(vet.as_str()) {
            continue;
        }
        if !day.is_empty() {
            let start_str = doc.get("start").and_then(Value::as_str).unwrap_or("");
            if !start_str.starts_with(&format!("{}T", day)) {
                continue;
            }
        }
        visits.push(json!({
            "id": e.id,
            "pet_id": doc.get("pet_id").cloned().unwrap_or(Value::Null),
            "vet": doc.get("vet").cloned().unwrap_or(Value::Null),
            "start": doc.get("start").cloned().unwrap_or(Value::Null),
            "minutes": doc.get("minutes").cloned().unwrap_or(Value::Null),
        }));
    }
    visits.sort_by(|a, b| a["start"].as_str().unwrap_or("").cmp(b["start"].as_str().unwrap_or("")));
    Reply::json(200, json!({"visits": visits}))
}

fn get_visit(id: &str) -> Reply {
    match records::get("visits", id) {
        Ok(entry) => {
            let doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
                return Reply::err(404, "not_found");
            }
            Reply::json(
                200,
                json!({
                    "id": entry.id,
                    "pet_id": doc.get("pet_id").cloned().unwrap_or(Value::Null),
                    "vet": doc.get("vet").cloned().unwrap_or(Value::Null),
                    "start": doc.get("start").cloned().unwrap_or(Value::Null),
                    "minutes": doc.get("minutes").cloned().unwrap_or(Value::Null),
                }),
            )
        }
        Err(records::StoreError::NotFound) => Reply::err(404, "not_found"),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn delete_visit(id: &str) -> Reply {
    match records::get("visits", id) {
        Ok(entry) => {
            let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
                return Reply::err(404, "not_found");
            }
            doc["deleted"] = json!(true);
            match records::update("visits", &entry.id, &doc.to_string(), entry.revision) {
                Ok(_) => Reply::no_content(),
                Err(_) => Reply::err(500, "store_error"),
            }
        }
        Err(records::StoreError::NotFound) => Reply::err(404, "not_found"),
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// Parses `YYYY-MM-DDTHH:MM:SSZ` into seconds since the Unix epoch. Only used
/// for ordering / overlap arithmetic — no timezone offsets, no leap seconds.
fn parse_rfc3339(s: &str) -> Option<i64> {
    if s.len() < 20 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}