//! Which queued goals a batch-start starts; pure, so testable without a fleet (ADR-0082).

use serde_json::Value;

/// Matches `platform-domain`'s `unwrap_or(100)`, so a ceiling cannot disagree with the listing.
const DEFAULT_PRIORITY: i64 = 100;

/// Keeps the platform's order (priority, then oldest); a malformed row is skipped, not fatal.
pub fn goals_to_start(rows: &[Value], max_priority: Option<i64>) -> Vec<String> {
    rows.iter()
        .filter(|row| row.get("state").and_then(Value::as_str) == Some("queued"))
        .filter(|row| match max_priority {
            None => true,
            Some(ceiling) => priority_of(row) <= ceiling,
        })
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn priority_of(row: &Value) -> i64 {
    match row.get("priority") {
        Some(p) => p.as_i64().or_else(|| p.as_f64().map(|f| f as i64)).unwrap_or(DEFAULT_PRIORITY),
        None => DEFAULT_PRIORITY,
    }
}
