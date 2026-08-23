//! What the clinic did, as a report. **This file is the goal of the `reports` part.**
//!
//! One pass over the day builds a single `Vec<Line>`; the CSV and the summary are
//! both projections of it, so the two endpoints can never disagree about which
//! visits belong to a day. The quoting is `csv:codec/codec::format`'s problem —
//! `Rex, Jr.` is exactly why.

use crate::bindings::csv::codec::codec;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use alloc_shim::BTreeMap;
use serde_json::{json, Value};

mod alloc_shim {
    pub use std::collections::BTreeMap;
}

/// One visit of the day, already joined to its pet.
struct Line {
    id: String,
    pet_id: String,
    pet_name: String,
    species: String,
    vet: String,
    start: String,
    minutes: u64,
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let rest = &route.segments[2.min(route.segments.len())..];
    let what = match (method, rest) {
        (Method::Get, [w]) => w.as_str(),
        _ => return Reply::err(404, "not_found"),
    };
    if what != "visits.csv" && what != "summary" {
        return Reply::err(404, "not_found");
    }

    let day = route.param("day");
    if !is_day(&day) {
        return Reply::err(400, "invalid");
    }
    let lines = match day_lines(&day) {
        Ok(l) => l,
        Err(r) => return r,
    };

    if what == "visits.csv" {
        Reply::raw(200, "text/csv", csv(&lines).into_bytes())
    } else {
        Reply::json(200, summary(&lines))
    }
}

/// `YYYY-MM-DD`, and a plausible calendar date.
fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    if !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return false;
    }
    let n = |a: usize, z: usize| s[a..z].parse::<u32>().unwrap_or(0);
    (1..=12).contains(&n(5, 7)) && (1..=31).contains(&n(8, 10))
}

fn day_lines(day: &str) -> Result<Vec<Line>, Reply> {
    let entries = match records::list_records("visits", 10_000, "") {
        Ok(p) => p.entries,
        Err(_) => return Err(Reply::err(500, "store_error")),
    };
    // One lookup per distinct pet, not per visit.
    let mut pets: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut lines: Vec<Line> = Vec::new();
    let prefix = format!("{}T", day);

    for e in &entries {
        let Ok(doc) = serde_json::from_str::<Value>(&e.data) else { continue };
        if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let start = doc.get("start").and_then(Value::as_str).unwrap_or("");
        if !start.starts_with(&prefix) {
            continue;
        }
        let pet_id = doc.get("pet_id").and_then(Value::as_str).unwrap_or("").to_string();
        let pet =
            pets.entry(pet_id.clone()).or_insert_with(|| match records::get("pets", &pet_id) {
                Ok(p) => {
                    let d: Value = serde_json::from_str(&p.data).unwrap_or_else(|_| json!({}));
                    (
                        d.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        d.get("species").and_then(Value::as_str).unwrap_or("").to_string(),
                    )
                }
                Err(_) => (String::new(), String::new()),
            });
        lines.push(Line {
            id: e.id.clone(),
            pet_id: pet_id.clone(),
            pet_name: pet.0.clone(),
            species: pet.1.clone(),
            vet: doc.get("vet").and_then(Value::as_str).unwrap_or("").to_string(),
            start: start.to_string(),
            minutes: doc.get("minutes").and_then(Value::as_u64).unwrap_or(0),
        });
    }
    lines.sort_by(|a, b| a.start.cmp(&b.start));
    Ok(lines)
}

fn csv(lines: &[Line]) -> String {
    let row = |f: [&str; 6]| codec::Row { fields: f.iter().map(|s| s.to_string()).collect() };
    let mut rows = vec![row(["id", "pet_id", "pet_name", "vet", "start", "minutes"])];
    for l in lines {
        let m = l.minutes.to_string();
        rows.push(row([&l.id, &l.pet_id, &l.pet_name, &l.vet, &l.start, &m]));
    }
    // The comma in `Rex, Jr.` is the codec's job, not `join(",")`'s.
    codec::format(
        &rows,
        &codec::Dialect { delimiter: ",".to_string(), has_header: true, trim: false },
    )
}

fn summary(lines: &[Line]) -> Value {
    // BTreeMap: serde here is built without std, so HashMap has no Serialize —
    // and sorted keys make the report deterministic anyway.
    let mut by_vet: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_species: BTreeMap<String, u32> = BTreeMap::new();
    let mut minutes: u64 = 0;
    for l in lines {
        minutes += l.minutes;
        *by_vet.entry(l.vet.clone()).or_insert(0) += 1;
        if !l.species.is_empty() {
            *by_species.entry(l.species.clone()).or_insert(0) += 1;
        }
    }
    json!({
        "visits": lines.len(),
        "minutes": minutes,
        "by_vet": by_vet,
        "by_species": by_species,
    })
}
