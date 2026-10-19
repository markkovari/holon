//! Which queued goals a batch-start should start, and in what order.
//!
//! Starting twenty related goals costs twenty `holon goal start <id>` round trips (ADR-0082: a human
//! decides to start a goal). This is the decision itself, kept pure: given the goal list from
//! `GET /api/projects/{p}/goals`, which ids should `/start` be called for.
//!
//! It does not call the platform or start anything; the CLI's `goal start <id>` loop does. Keeping the
//! decision pure makes it testable in milliseconds with no fleet.

use serde_json::Value;

/// The priority the platform gives a goal created without one.
///
/// `components/platform-domain/src/goals.rs` uses `b.priority.unwrap_or(100)`: matched here exactly, so
/// a ceiling cannot disagree with the listing.
const DEFAULT_PRIORITY: i64 = 100;

/// The ids of the queued goals a batch-start should start, in the order the platform listed them.
/// Only rows whose `"state"` is exactly `"queued"` are returned; any other or malformed state is
/// excluded.
///
/// When `max_priority` is `Some(n)`, a queued goal is included only if its `"priority"`, read as an
/// integer, is `<= n`; a missing or non-numeric priority counts as [`DEFAULT_PRIORITY`]. When `None`,
/// every queued goal is included.
///
/// Input order is preserved: the platform's `goals_list` already returns priority-first-then-oldest,
/// and re-sorting here would be a second place to disagree with it.
///
/// A queued row with no string `"id"` is skipped silently: one malformed row must not cost the rest of
/// the batch. An empty `rows` slice returns an empty `Vec`.
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

/// A goal's priority, defaulting to [`DEFAULT_PRIORITY`] when absent or not a number, as the platform
/// does at creation.
fn priority_of(row: &Value) -> i64 {
    match row.get("priority") {
        Some(p) => p.as_i64().or_else(|| p.as_f64().map(|f| f as i64)).unwrap_or(DEFAULT_PRIORITY),
        None => DEFAULT_PRIORITY,
    }
}
