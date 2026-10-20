//! Each refusal is an early `Err`, so the contract's validation order is the line order.

use crate::bindings::p3::http::types::Method;
use crate::bindings::pii::redact::redactor as pii;
use crate::bindings::records::store::store as records;
use crate::{Reply, Route};
use serde_json::{json, Map, Value};

const COLLECTION: &str = "requests";
/// A finished job does not block a new duplicate.
const LIVE: [&str; 3] = ["new", "assigned", "enroute"];

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    let out = match seg.as_slice() {
        ["api", "requests"] => match method {
            Method::Post => create(body),
            Method::Get => list(route),
            _ => Err(Reply::err(405, "method_not_allowed")),
        },
        ["api", "requests", id] => match method {
            Method::Get => one(id),
            _ => Err(Reply::err(405, "method_not_allowed")),
        },
        _ => Err(Reply::err(404, "not_found")),
    };
    out.unwrap_or_else(|e| e)
}

fn create(body: &str) -> Result<Reply, Reply> {
    let input: Value = serde_json::from_str(body).map_err(|_| invalid())?;

    let title = input
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(invalid)?
        .to_string();
    let lat = number(&input, "lat")?;
    let lon = number(&input, "lon")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(Reply::err(400, "bad_coordinate"));
    }

    if let Some(existing) = duplicate_of(&title, lat, lon)? {
        return Err(Reply::json(409, json!({ "error": "duplicate", "existing": existing })));
    }

    // Masked before anything can persist it.
    let notes = pii::redact(
        input.get("notes").and_then(Value::as_str).unwrap_or(""),
        &pii::Options { kinds: Vec::new() },
    );

    let mut doc = json!({
        "title": title,
        "notes": notes,
        "lat": lat,
        "lon": lon,
        "state": "new",
        "engineer": "",
        "distance_m": 0,
        "created": "",
    });

    // `created` is the store's stamp, which exists only after create.
    let entry = records::create(
        COLLECTION,
        &doc.to_string(),
        &["state".to_string(), "engineer".to_string()],
    )
    .map_err(|_| store_error())?;

    doc["created"] = json!(rfc3339(entry.created));
    let entry = records::update(COLLECTION, &entry.id, &doc.to_string(), entry.revision)
        .map_err(|_| store_error())?;

    Ok(Reply::json(201, merged(&entry)))
}

fn number(input: &Value, key: &str) -> Result<f64, Reply> {
    input.get(key).and_then(Value::as_f64).ok_or_else(invalid)
}

fn invalid() -> Reply {
    Reply::err(400, "invalid")
}

fn store_error() -> Reply {
    Reply::err(500, "store_error")
}

fn duplicate_of(title: &str, lat: f64, lon: f64) -> Result<Option<String>, Reply> {
    for state in LIVE {
        // Indexed by JSON encoding: `new` is `"new"`.
        let filters = vec![records::Filter {
            field: "state".to_string(),
            value: Value::String(state.to_string()).to_string(),
        }];
        let entries = records::query(COLLECTION, &filters, 10_000).map_err(|_| store_error())?;
        for e in entries {
            let d = parse(&e.data);
            let same = d.get("title").and_then(Value::as_str) == Some(title)
                && close(d.get("lat"), lat)
                && close(d.get("lon"), lon);
            if same {
                return Ok(Some(e.id));
            }
        }
    }
    Ok(None)
}

/// Within a tolerance: coordinates come back through JSON.
fn close(v: Option<&Value>, want: f64) -> bool {
    v.and_then(Value::as_f64).is_some_and(|got| (got - want).abs() < 1e-9)
}

fn list(route: &Route) -> Result<Reply, Reply> {
    let mut filters = Vec::new();
    for key in ["state", "engineer"] {
        let v = route.param(key);
        if !v.is_empty() {
            filters.push(records::Filter {
                field: key.to_string(),
                value: Value::String(v).to_string(),
            });
        }
    }

    let entries = if filters.is_empty() {
        every()?
    } else {
        records::query(COLLECTION, &filters, 10_000).map_err(|_| store_error())?
    };

    let mut docs: Vec<Value> = entries.iter().map(merged).collect();
    docs.sort_by_key(key_of);
    Ok(Reply::json(200, json!({ "requests": docs })))
}

fn key_of(d: &Value) -> (String, String) {
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    (s("created"), s("id"))
}

fn one(id: &str) -> Result<Reply, Reply> {
    match records::get(COLLECTION, id) {
        Ok(e) => Ok(Reply::json(200, merged(&e))),
        Err(_) => Err(Reply::err(404, "not_found")),
    }
}

/// Walked, not filtered: no filter means no index to ask.
fn every() -> Result<Vec<records::Entry>, Reply> {
    let mut out = Vec::new();
    let mut after = String::new();
    // ponytail: a 100-page cap, not an unbounded loop; raise it past 100k requests a day.
    for _ in 0..100 {
        let page = records::list_records(COLLECTION, 1000, &after).map_err(|_| store_error())?;
        let empty = page.entries.is_empty();
        out.extend(page.entries);
        if empty || page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    Ok(out)
}

fn parse(data: &str) -> Value {
    serde_json::from_str(data).unwrap_or_else(|_| json!({}))
}

fn merged(e: &records::Entry) -> Value {
    let mut map: Map<String, Value> = match parse(&e.data) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    map.insert("id".to_string(), json!(e.id));
    Value::Object(map)
}

/// No wall clock in this world: days-from-civil backwards (Hinnant).
fn rfc3339(stamp: u64) -> String {
    // The store's unit is not in the contract, so normalise by magnitude.
    let secs = match stamp {
        s if s >= 1_000_000_000_000_000_000 => s / 1_000_000_000,
        s if s >= 1_000_000_000_000_000 => s / 1_000_000,
        s if s >= 1_000_000_000_000 => s / 1_000,
        s => s,
    };
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_render_as_rfc3339_utc() {
        assert_eq!(rfc3339(1_756_800_000), "2025-09-02T08:00:00Z");
        assert_eq!(rfc3339(1_756_800_000_000), rfc3339(1_756_800_000));
        assert_eq!(rfc3339(1_756_800_000_000_000_000), rfc3339(1_756_800_000));
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn coordinates_compare_through_json() {
        assert!(close(Some(&json!(47.479)), 47.479));
        assert!(!close(Some(&json!(47.479)), 47.48));
        assert!(!close(None, 0.0));
    }
}
