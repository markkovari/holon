//! `comp-goald` must tell "the gate refused to run" apart from a real search exhaustion or success: a
//! REFUSED gate (a check's command not in the tree yet) exited 0 like success, so `comp-goald` reported
//! `awaiting-human` with no branch run and no PR opened (see the `ponytail:` note beside
//! `gate_can_judge`'s call site in `reconciler/src/bin/goalrun.rs`).
//!
//! The held-out judge for that fix: `comp_reconciler::goalexit` must exist with these exact names and
//! behavior. A plain unit test of a pure module, so it stays fast and reliable.

use comp_reconciler::goalexit;

#[test]
fn success_is_the_reserved_zero() {
    // `comp-goald`'s `status.success()` check depends on this staying 0 (the OS's own convention).
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
    // `ExitStatus::code()` is `None` when a process died to a signal; this must not unwrap and crash the
    // daemon reporting on ANOTHER goal's outcome.
    let reason = goalexit::failure_reason(None);
    assert!(!reason.is_empty());
}
