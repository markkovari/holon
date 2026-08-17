//! What the clinic did, as a report. **This file is the goal of the `reports` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## The capability you must not reimplement
//!
//! `crate::bindings::csv::codec::codec` formats CSV — `format(rows, dialect)`,
//! where a row is a list of fields. It already knows that a field containing a
//! comma, a quote or a newline has to be quoted and its quotes doubled. A pet in
//! this clinic is called `Rex, Jr.` precisely so that `join(",")` produces a file
//! with the wrong number of columns, and the gate reads the column count.
//!
//! The other two halves are already written and are yours to read from: owners,
//! pets and visits are all in `records:store` under those collection names.
use crate::bindings::csv::codec::codec;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One visit of the day, already joined against its pet.
struct Line {
    start: String,
    fields: [String; 6], // id, pet_id, pet_name, vet, start, minutes
    species: String,
    vet: String,
    minutes: u64,
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let rest = &route.segments[2.min(route.segments.len())..];
    let what = match (method, rest) {
        (Method::Get, [one]) => one.as_str(),
        _ => return Reply::err(404, "not_found"),
    };
    if !matches!(what, "visits.csv" | "summary") {
        return Reply::err(404, "not_found");
    }
    let day = route.param("day");
    if !is_day(&day) {
        return Reply::err(400, "invalid");
    }
    let lines = match gather(&day) {
        Ok(l) => l,
        Err(r) => return r,
    };
    match what {
        "visits.csv" => csv(lines),
        _ => summary(lines),
    }
}

/// `YYYY-MM-DD`, and a real calendar date rather than `2026-13-45`.
fn is_day(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if (y.len(), m.len(), d.len()) != (4, 2, 2) {
        return false;
    }
    let (Ok(_y), Ok(m), Ok(d)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return false;
    };
    (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// The day's visits, joined to their pets, sorted by `start`.
///
/// One pass for both endpoints — the CSV and the summary disagreeing about which
/// visits happened that day is the failure mode worth designing out.
fn gather(day: &str) -> Result<Vec<Line>, Reply> {
    let entries = match records::list_records("visits", 10_000, "") {
        Ok(p) => p.entries,
        Err(_) => return Err(Reply::err(500, "store_error")),
    };
    // ponytail: pets fetched one-by-one behind this cache; a batch read if the
    // store ever grows one.
    let mut pets: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut lines = Vec::new();
    for e in entries {
        let Ok(doc) = serde_json::from_str::<Value>(&e.data) else { continue };
        if doc.get("deleted").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let start = doc.get("start").and_then(Value::as_str).unwrap_or("").to_string();
        if !start.starts_with(day) || start.as_bytes().get(day.len()) != Some(&b'T') {
            continue;
        }
        let pet_id = doc.get("pet_id").and_then(Value::as_str).unwrap_or("").to_string();
        let (name, species) = pets
            .entry(pet_id.clone())
            .or_insert_with(|| match records::get("pets", &pet_id) {
                Ok(p) => {
                    let d: Value = serde_json::from_str(&p.data).unwrap_or(Value::Null);
                    (
                        d.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        d.get("species").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                    )
                }
                Err(_) => (String::new(), "unknown".to_string()),
            })
            .clone();
        let vet = doc.get("vet").and_then(Value::as_str).unwrap_or("").to_string();
        let minutes = doc.get("minutes").and_then(Value::as_u64).unwrap_or(0);
        lines.push(Line {
            fields: [
                e.id.clone(),
                pet_id,
                name,
                vet.clone(),
                start.clone(),
                minutes.to_string(),
            ],
            start,
            species,
            vet,
            minutes,
        });
    }
    lines.sort_by(|a, b| a.start.cmp(&b.start));
    Ok(lines)
}

fn csv(lines: Vec<Line>) -> Reply {
    // The header is just the first row; `format` does the quoting, which is the
    // whole point — `Rex, Jr.` has to come back as one quoted field.
    let rows: Vec<codec::Row> = core::iter::once(codec::Row {
        fields: ["id", "pet_id", "pet_name", "vet", "start", "minutes"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
    .chain(lines.into_iter().map(|l| codec::Row { fields: l.fields.to_vec() }))
    .collect();
    let doc = codec::format(
        &rows,
        &codec::Dialect { delimiter: ",".to_string(), has_header: true, trim: false },
    );
    Reply::raw(200, "text/csv", doc.into_bytes())
}

fn summary(lines: Vec<Line>) -> Reply {
    // BTreeMap, not HashMap: serde here is built without std, so HashMap has no
    // Serialize impl — and the key order comes out deterministic for free.
    let mut by_vet: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_species: BTreeMap<String, u32> = BTreeMap::new();
    let mut minutes: u64 = 0;
    for l in &lines {
        minutes += l.minutes;
        *by_vet.entry(l.vet.clone()).or_default() += 1;
        *by_species.entry(l.species.clone()).or_default() += 1;
    }
    Reply::json(
        200,
        json!({
            "visits": lines.len(),
            "minutes": minutes,
            "by_vet": by_vet,
            "by_species": by_species,
        }),
    )
}
