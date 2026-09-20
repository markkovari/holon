//! Which queued goals a batch-start should start, and in what order.
//!
//! ADR-0082 is right that a human decides to start a goal — but today that
//! decision costs one `holon goal start <id>` round trip per goal, so starting
//! twenty related goals is twenty repetitions of the same click rather than one
//! deliberate act covering all twenty. This is the decision itself, kept pure:
//! given the goal list the platform's `GET /api/projects/{p}/goals` already
//! returns, which ids should `/start` be called for.
//!
//! It does not call the platform, open a socket, or start anything. The CLI's
//! own `goal start <id>` loop calls `/start` for each id this returns. Keeping
//! the decision pure is what makes it testable with no fleet, no compose and no
//! host, in milliseconds.

use serde_json::Value;

/// The priority the platform gives a goal created without one.
///
/// `components/platform-domain/src/goals.rs` uses `b.priority.unwrap_or(100)`,
/// so a goal that arrives with no priority means 100 on both sides. Matched
/// here exactly, not chosen — a ceiling that disagreed with the listing would
/// be a second, quieter definition of "prioritised".
const DEFAULT_PRIORITY: i64 = 100;

/// The ids of the queued goals a batch-start should start, in the order the
/// platform listed them.
///
/// Only rows whose `"state"` is exactly the string `"queued"` are ever
/// returned; every other state (`"running"`, `"awaiting-human"`, `"done"`,
/// `"failed"`, `"abandoned"`) and a missing or malformed `state` is excluded.
///
/// When `max_priority` is `Some(n)`, a queued goal is included only if its
/// `"priority"`, read as an integer, is `<= n`; a missing or non-numeric
/// priority counts as [`DEFAULT_PRIORITY`]. When it is `None`, every queued
/// goal is included regardless of priority.
///
/// Input order is preserved deliberately — no sort, no reverse. The platform's
/// own `goals_list` already returns goals priority-first-then-oldest, so a
/// caller that fetches the list and passes it straight through gets the right
/// order for free; re-sorting here would be a second place that could disagree
/// with it.
///
/// A queued row with no `"id"` (or a non-string one) is skipped silently:
/// there is nothing useful to return for it, and one malformed row must not
/// cost the rest of the batch. An empty `rows` slice returns an empty `Vec`.
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

/// A goal's priority, defaulting to [`DEFAULT_PRIORITY`] when the field is
/// absent or is not a number — the same default the platform applies when a
/// goal is created, so "no priority" means one thing, not two.
fn priority_of(row: &Value) -> i64 {
    match row.get("priority") {
        Some(p) => p
            .as_i64()
            .or_else(|| p.as_f64().map(|f| f as i64))
            .unwrap_or(DEFAULT_PRIORITY),
        None => DEFAULT_PRIORITY,
    }
}