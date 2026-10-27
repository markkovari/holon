//! Held-out judge for `holon_goal::goalexit`: a refused gate once exited 0 like success.

use holon_goal::goalexit;

#[test]
fn success_is_the_reserved_zero() {
    // `comp-goald`'s `status.success()` depends on this staying 0.
    assert_eq!(goalexit::SUCCESS, 0);
}

#[test]
fn gate_refused_is_a_distinct_code_from_success_and_exhausted() {
    assert_ne!(goalexit::GATE_REFUSED, goalexit::SUCCESS);
    assert_ne!(goalexit::GATE_REFUSED, goalexit::EXHAUSTED);
    assert_ne!(goalexit::EXHAUSTED, goalexit::SUCCESS);
}

#[test]
fn a_refused_gate_and_a_real_exhaustion_read_differently() {
    let exhausted = goalexit::failure_reason(Some(goalexit::EXHAUSTED));
    let refused = goalexit::failure_reason(Some(goalexit::GATE_REFUSED));
    assert_ne!(exhausted, refused, "two different failures must not read the same");
    assert!(
        exhausted.contains("no branch passed"),
        "exhausted's reason lost its meaning: {exhausted}"
    );
    assert!(
        refused.contains("nothing was spent"),
        "refused's reason must say nothing was spent, so a person reading it \
         knows not to treat this like a real search result: {refused}"
    );
}

#[test]
fn an_unrecognised_code_still_names_the_number() {
    let reason = goalexit::failure_reason(Some(17));
    assert!(reason.contains("17"), "an operator debugging this needs the actual code: {reason}");
}

#[test]
fn a_signal_kill_reports_something_rather_than_panicking() {
    // A signal death is `None`; unwrapping would crash the daemon reporting another goal.
    let reason = goalexit::failure_reason(None);
    assert!(!reason.is_empty());
}
