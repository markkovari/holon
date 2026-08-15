//! The specification, held out: this file is not writable by the goal.
//!
//! Every case here is a rule people get wrong, and most of them are the
//! pre-release rules — which is the point. A range matcher that handles `^1.2.3`
//! and nothing else looks finished and is not.

use semver_range::matches;

#[test]
fn caret_is_compatible_within_the_leftmost_nonzero() {
    assert!(matches("^1.2.3", "1.2.3"));
    assert!(matches("^1.2.3", "1.9.9"));
    assert!(!matches("^1.2.3", "2.0.0"));
    assert!(!matches("^1.2.3", "1.2.2"));
    // 0.x is minor-locked: the leftmost nonzero is the MINOR.
    assert!(matches("^0.2.3", "0.2.4"));
    assert!(!matches("^0.2.3", "0.3.0"));
    // 0.0.x is patch-locked.
    assert!(matches("^0.0.3", "0.0.3"));
    assert!(!matches("^0.0.3", "0.0.4"));
}

#[test]
fn tilde_allows_patch_and_minor_only_when_the_minor_is_absent() {
    assert!(matches("~1.2.3", "1.2.9"));
    assert!(!matches("~1.2.3", "1.3.0"));
    assert!(matches("~1.2", "1.2.9"));
    assert!(!matches("~1.2", "1.3.0"));
}

#[test]
fn comparators_conjoin() {
    assert!(matches(">=1.0.0, <2.0.0", "1.5.0"));
    assert!(!matches(">=1.0.0, <2.0.0", "2.0.0"));
    assert!(!matches(">=1.0.0, <2.0.0", "0.9.9"));
    assert!(matches("*", "3.1.4"));
}

#[test]
fn a_prerelease_is_below_its_release() {
    assert!(matches("<1.0.0", "1.0.0-alpha"));
    assert!(!matches(">=1.0.0", "1.0.0-alpha"));
}

#[test]
fn prerelease_identifiers_compare_by_kind_then_value() {
    // Numeric identifiers compare NUMERICALLY, so 2 < 10 — the case a
    // string comparison gets backwards.
    assert!(matches("<1.0.0-alpha.10", "1.0.0-alpha.2"));
    // Alphanumeric identifiers compare in ASCII order, and an alphanumeric
    // identifier outranks a numeric one.
    assert!(matches("<1.0.0-alpha.beta", "1.0.0-alpha.1"));
    assert!(matches("<1.0.0-beta", "1.0.0-alpha.beta"));
    // A shorter set of identifiers is lower when all preceding ones are equal.
    assert!(matches("<1.0.0-alpha.1", "1.0.0-alpha"));
}

#[test]
fn a_prerelease_satisfies_a_range_only_when_the_range_names_one() {
    // THE rule everybody misses: `^1.0.0` does not admit `1.1.0-alpha`, even
    // though 1.1.0-alpha is inside the version interval, because a range with no
    // pre-release in it does not opt into pre-releases at all.
    assert!(!matches("^1.0.0", "1.1.0-alpha"));
    // It does admit one whose [major, minor, patch] the range itself named.
    assert!(matches(">=1.0.0-alpha, <2.0.0", "1.0.0-beta"));
    assert!(!matches(">=1.0.0-alpha, <2.0.0", "1.2.0-beta"));
}

#[test]
fn build_metadata_is_not_precedence() {
    assert!(matches("1.0.0", "1.0.0+build.5"));
    assert!(matches("^1.0.0", "1.2.3+sha.abc"));
}

#[test]
fn nonsense_does_not_panic() {
    assert!(!matches("", "1.0.0"));
    assert!(!matches("^1.0.0", ""));
    assert!(!matches("^1.0.0", "not-a-version"));
    assert!(!matches("!!!", "1.0.0"));
}
