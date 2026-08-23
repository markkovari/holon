//! The day's reports, counted and tabulated.
//!
//! One decomposition instead of two: the CSV table IS the intermediate
//! representation. `table()` builds the five columns once, sorted; the JSON
//! digest is then read back out of those columns (`tally(col)`), so the summary
//! and the CSV cannot disagree about what happened that day. `csv:codec::format`
//! does the quoting — `Login fails, silently` keeps its five columns.
use crate::bindings::csv::codec::codec as csv;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::json;
use std::collections::BTreeMap;

const COLUMNS: [&str; 5] = ["id", "title", "component", "state", "severity"];
const ID: usize = 0;
const COMPONENT: usize = 2;
const STATE: usize = 3;
const SEVERITY: usize = 4;

/// `YYYY-MM-DD` and nothing else. No chrono: a date this API only ever compares
/// as a string prefix does not need to become a date.
fn valid_day(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && (1..=12).contains(&d[5..7].parse::<u8>().unwrap_or(0))
        && (1..=31).contains(&d[8..10].parse::<u8>().unwrap_or(0))
}

/// The day's reports as the CSV body rows, sorted by `reported_at` then `id`.
///
/// ponytail: whole-collection walk. `reported_at` is not an index field in the
/// contract, so `query` would scan anyway; index it and switch to `find-by` if a
/// day ever costs more than a page or two.
fn table(day: &str) -> Vec<Vec<String>> {
    let mut keyed: Vec<(String, Vec<String>)> = Vec::new();
    let mut after = String::new();
    loop {
        let Ok(page) = records::list_records("reports", 0, &after) else { break };
        for e in &page.entries {
            let doc: serde_json::Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            let field = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let reported = field("reported_at");
            if !reported.starts_with(day) {
                continue;
            }
            keyed.push((
                reported,
                vec![
                    e.id.clone(),
                    field("title"),
                    field("component"),
                    field("state"),
                    field("severity"),
                ],
            ));
        }
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    keyed.sort_by(|a, b| (&a.0, &a.1[ID]).cmp(&(&b.0, &b.1[ID])));
    keyed.into_iter().map(|(_, fields)| fields).collect()
}

/// Count one column's values, skipping blanks — which is exactly "only the keys
/// that occur", with no zero-filling to undo afterwards.
fn tally(rows: &[Vec<String>], col: usize) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for r in rows {
        if !r[col].is_empty() {
            *counts.entry(r[col].clone()).or_insert(0) += 1;
        }
    }
    counts
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Get) {
        return Reply::err(404, "not_found");
    }
    let day = route.param("day");
    if !valid_day(&day) {
        return Reply::err(400, "invalid");
    }
    let rows = table(&day);
    let wants_csv = route.segments.last().map(|s| s == "digest.csv").unwrap_or(false);

    if wants_csv {
        let mut doc = vec![csv::Row { fields: COLUMNS.iter().map(|c| c.to_string()).collect() }];
        doc.extend(rows.into_iter().map(|fields| csv::Row { fields }));
        let dialect = csv::Dialect { delimiter: ",".to_string(), has_header: true, trim: false };
        return Reply::raw(200, "text/csv", csv::format(&doc, &dialect).into_bytes());
    }

    Reply::json(
        200,
        json!({
            "day": day,
            "total": rows.len(),
            "by_state": tally(&rows, STATE),
            "by_component": tally(&rows, COMPONENT),
            "open_high": rows.iter().filter(|r| r[SEVERITY] == "high" && r[STATE] != "closed").count(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, component: &str, state: &str, severity: &str) -> Vec<String> {
        vec![id, "t", component, state, severity].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn day_and_counts() {
        assert!(valid_day("2026-08-17"));
        for bad in ["", "2026-8-17", "2026-13-01", "2026-08-32", "2026-08-17T00:00:00Z"] {
            assert!(!valid_day(bad), "{bad}");
        }
        let rows = vec![
            row("a", "auth", "open", "high"),
            row("b", "auth", "closed", ""),
            row("c", "billing", "open", "high"),
        ];
        assert_eq!(
            tally(&rows, COMPONENT),
            BTreeMap::from([("auth".into(), 2), ("billing".into(), 1)])
        );
        // no zero-filled keys, and the blank severity is absent rather than counted
        assert_eq!(tally(&rows, SEVERITY), BTreeMap::from([("high".into(), 2)]));
        assert_eq!(
            rows.iter().filter(|r| r[SEVERITY] == "high" && r[STATE] != "closed").count(),
            2
        );
    }
}
