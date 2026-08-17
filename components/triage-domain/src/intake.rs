//! Taking a defect report in. **This file is the goal of the `intake` part.**
//!
//! `body` is stored MASKED, via `pii:redact` — the raw text never reaches the
//! store, because the digest is readable by anyone.
//!
//! Everything that reads reports here goes through ONE store call, `query`:
//! it takes a list of filters, ANDs them, and re-checks each candidate against
//! every filter — so the `state`+`component` list filter, the "no filter at all"
//! list, and the duplicate lookup (`component` + `title`, and `title` is not even
//! indexed) are the same three lines with a different filter vector. `find_by`
//! would have needed two calls and a hand-written intersection for the list, and a
//! second pass for the duplicate.
//!
//! `query` indexes and compares the JSON ENCODING of a value, so a string field
//! `billing` is `"billing"`, quotes included — `filter()` does that encoding, and
//! nothing here passes a bare string.
use crate::bindings::pii::redact::redactor as pii;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Map, Value};

/// A ceiling far above any test collection; `query` applies no cap of its own and
/// a limit of 0 would silently mean "50".
const LIMIT: u32 = 10_000;

const INDEXES: [&str; 2] = ["component", "state"];

/// A filter on the JSON encoding of a string value — the form the store indexes.
fn filter(field: &str, value: &str) -> records::Filter {
    records::Filter { field: field.to_string(), value: Value::String(value.to_string()).to_string() }
}

/// The stored document with the store's id merged in.
fn doc(entry: &records::Entry) -> Value {
    let mut map: Map<String, Value> = serde_json::from_str(&entry.data).unwrap_or_default();
    map.insert("id".to_string(), json!(entry.entry_id()));
    Value::Object(map)
}

/// `Entry` has no method for this; a named helper keeps `doc` honest about it.
trait EntryId {
    fn entry_id(&self) -> &str;
}
impl EntryId for records::Entry {
    fn entry_id(&self) -> &str {
        &self.id
    }
}

fn field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Epoch seconds → RFC3339 UTC. The world has no wall clock, so the timestamp
/// comes from the store's own `created` on the record it just wrote.
fn rfc3339(secs: u64) -> String {
    let (days, tod) = ((secs / 86400) as i64, secs % 86400);
    // civil-from-days: era arithmetic, no lookup tables, no leap-year special case.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod / 60) % 60,
        tod % 60
    )
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "reports"]) => create(body),
        (Method::Get, ["api", "reports"]) => list(route),
        (Method::Get, ["api", "reports", id]) => match records::get("reports", id) {
            Ok(e) => Reply::json(200, doc(&e)),
            Err(_) => Reply::err(404, "not_found"),
        },
        _ => Reply::err(404, "not_found"),
    }
}

fn create(body: &str) -> Reply {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "invalid");
    };
    let (Some(title), Some(raw), Some(component)) =
        (field(&v, "title"), field(&v, "body"), field(&v, "component"))
    else {
        return Reply::err(400, "invalid");
    };

    // Duplicate: same component AND same title, on a report that is not closed. A
    // closed one does not block — the bug came back.
    let existing = records::query(
        "reports",
        &[filter("component", &component), filter("title", &title)],
        LIMIT,
    )
    .unwrap_or_default()
    .into_iter()
    .find(|e| doc(e).get("state").and_then(Value::as_str) != Some("closed"));
    if let Some(e) = existing {
        return Reply::json(409, json!({ "error": "duplicate", "existing": e.id }));
    }

    // Masked before it is written: emails, cards, SSNs, phones, IPs. Empty `kinds`
    // is every kind, which is what a body pasted by a reporter needs.
    let masked = pii::redact(&raw, &pii::Options { kinds: Vec::new() });

    let mut document = json!({
        "title": title,
        "body": masked,
        "component": component,
        "state": "open",
    });
    let indexes = INDEXES.map(str::to_string);
    let Ok(entry) = records::create("reports", &document.to_string(), &indexes) else {
        return Reply::err(500, "store_error");
    };

    // `reported_at` can only be filled in once the record exists: its timestamp is
    // the store's `created`, and this world imports no wall clock.
    document["reported_at"] = json!(rfc3339(entry.created));
    if records::update("reports", &entry.id, &document.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    let mut out = document;
    out["id"] = json!(entry.id);
    out.as_object_mut().map(|m| m.remove("reported_at"));
    Reply::json(201, out)
}

fn list(route: &Route) -> Reply {
    let filters: Vec<records::Filter> = ["state", "component"]
        .iter()
        .filter_map(|k| match route.param(k) {
            v if v.is_empty() => None,
            v => Some(filter(k, &v)),
        })
        .collect();
    match records::query("reports", &filters, LIMIT) {
        Ok(entries) => {
            let mut reports: Vec<Value> = entries.iter().map(doc).collect();
            reports.sort_by_key(|r| {
                (
                    r.get("reported_at").and_then(Value::as_str).unwrap_or("").to_string(),
                    r.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                )
            });
            Reply::json(200, json!({ "reports": reports }))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00Z");
        // A leap day, past the century rule the era arithmetic exists for.
        assert_eq!(super::rfc3339(1_582_934_400), "2020-02-29T00:00:00Z");
        assert_eq!(super::rfc3339(1_755_421_200), "2025-08-17T09:00:00Z");
    }
}