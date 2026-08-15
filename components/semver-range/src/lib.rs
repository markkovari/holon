//! `semver-range` — does a version satisfy a range?
//!
//! A stub. `tests/ranges.rs` is the specification.

/// Does `version` satisfy `range`?
///
/// Ranges: `*`, `1.2.3`, `^1.2.3`, `~1.2.3`, `~1.2`, `>=1.0.0`, `<2.0.0`, and
/// comma-separated conjunctions of those.
pub fn matches(_range: &str, _version: &str) -> bool {
    false
}
