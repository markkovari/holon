//! Which queued goals a batch-start should start — the pure decision behind
//! `holon goal start --queued <project>` (not built yet; this is the piece
//! it will call). Found talking through "if I want to start 20 goals at
//! once": today that costs twenty individual `holon goal start <id>`
//! round trips, one per goal, because nothing decides "these are the ones"
//! in one place.
//!
//! This is the held-out judge: `comp_reconciler::goalbatch::goals_to_start`
//! must exist with this exact signature and this exact behavior. Deliberately
//! a plain unit test of a pure module — no fleet, no compose, no host — so
//! this stays fast and reliable, the same discipline `goalexit.rs` uses.

use comp_reconciler::goalbatch::goals_to_start;
use serde_json::json;

#[test]
fn only_queued_goals_are_ever_included() {
    let rows = vec![
        json!({ "id": "a", "state": "queued", "priority": 100 }),
        json!({ "id": "b", "state": "running", "priority": 100 }),
        json!({ "id": "c", "state": "awaiting-human", "priority": 100 }),
        json!({ "id": "d", "state": "done", "priority": 100 }),
        json!({ "id": "e", "state": "failed", "priority": 100 }),
        json!({ "id": "f", "state": "abandoned", "priority": 100 }),
        json!({ "id": "g", "state": "queued", "priority": 100 }),
    ];
    assert_eq!(goals_to_start(&rows, None), vec!["a".to_string(), "g".to_string()]);
}

#[test]
fn a_missing_or_malformed_state_is_excluded_not_started() {
    let rows = vec![
        json!({ "id": "a", "priority": 100 }),
        json!({ "id": "b", "state": 5, "priority": 100 }),
        json!({ "id": "c", "state": "queued", "priority": 100 }),
    ];
    assert_eq!(goals_to_start(&rows, None), vec!["c".to_string()]);
}

#[test]
fn no_ceiling_starts_every_queued_goal_regardless_of_priority() {
    let rows = vec![
        json!({ "id": "a", "state": "queued", "priority": 1 }),
        json!({ "id": "b", "state": "queued", "priority": 999 }),
    ];
    assert_eq!(goals_to_start(&rows, None), vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn a_priority_ceiling_excludes_anything_above_it() {
    let rows = vec![
        json!({ "id": "a", "state": "queued", "priority": 10 }),
        json!({ "id": "b", "state": "queued", "priority": 50 }),
        json!({ "id": "c", "state": "queued", "priority": 51 }),
    ];
    assert_eq!(goals_to_start(&rows, Some(50)), vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn a_priority_equal_to_the_ceiling_is_included() {
    let rows = vec![json!({ "id": "a", "state": "queued", "priority": 50 })];
    assert_eq!(goals_to_start(&rows, Some(50)), vec!["a".to_string()]);
}

#[test]
fn a_missing_priority_defaults_to_100_matching_the_platform() {
    // components/platform-domain/src/goals.rs: `b.priority.unwrap_or(100)`
    // — a goal created with none gets 100 there, so this must agree.
    let rows = vec![json!({ "id": "a", "state": "queued" })];
    assert_eq!(
        goals_to_start(&rows, Some(100)),
        vec!["a".to_string()],
        "100 should just clear a ceiling of 100"
    );
    assert_eq!(
        goals_to_start(&rows, Some(99)),
        Vec::<String>::new(),
        "100 should not clear a ceiling of 99"
    );
}

#[test]
fn the_input_order_is_preserved_not_resorted() {
    // The platform's own listing already returns priority-first-then-oldest
    // (`goals_list`) — a caller passing that straight through must get the
    // same order back, not a second, possibly-disagreeing sort.
    let rows = vec![
        json!({ "id": "z", "state": "queued", "priority": 5 }),
        json!({ "id": "a", "state": "queued", "priority": 1 }),
        json!({ "id": "m", "state": "queued", "priority": 5 }),
    ];
    assert_eq!(
        goals_to_start(&rows, None),
        vec!["z".to_string(), "a".to_string(), "m".to_string()],
        "already-sorted input must come back in the same order, not re-sorted by priority"
    );
}

#[test]
fn a_queued_goal_with_no_id_is_skipped_not_a_hard_error() {
    let rows = vec![
        json!({ "state": "queued", "priority": 100 }),
        json!({ "id": "b", "state": "queued", "priority": 100 }),
    ];
    assert_eq!(goals_to_start(&rows, None), vec!["b".to_string()]);
}

#[test]
fn an_empty_list_starts_nothing() {
    assert_eq!(goals_to_start(&[], None), Vec::<String>::new());
    assert_eq!(goals_to_start(&[], Some(50)), Vec::<String>::new());
}
