//! Requirement checking and `auto-v1` scoring (CONTRACT.md "Requirements",
//! "Timed competitions"). Pure functions over a stored photo record: no I/O, so
//! every rule is unit-tested here and quests and competitions judge the same way.
//! Scaffold: the signatures are the contract between `progress.rs`,
//! `competitions.rs` and this file.

use serde_json::{json, Map, Value};

/// Judge `photo` against `requirements`. `starts_at` is the quest's or
/// competition's start, for `captured_after_start`.
pub fn verdict(requirements: &Value, photo: &Map<String, Value>, starts_at: u64) -> Value {
    let _ = (requirements, photo, starts_at);
    json!({ "pass": false, "checks": [ { "name": "rules", "ok": null, "detail": "not implemented" } ] })
}

/// Refuse a malformed requirements object before it is stored (`bad_requirements`).
pub fn validate(requirements: &Value) -> Result<(), String> {
    let _ = requirements;
    Ok(())
}

/// `auto-v1`, 0..=1, plus flags for parts that could not be computed.
pub fn auto_v1(photo: &Map<String, Value>) -> (f64, Vec<String>) {
    let _ = photo;
    (0.0, vec!["not implemented".into()])
}
