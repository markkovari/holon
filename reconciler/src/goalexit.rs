//! The exit codes `comp-goalrun` returns, and what each one means to `comp-goald`.
//!
//! A run ending is three different things wearing one face:
//!
//!   * a candidate passed the gate and a pull request was opened — success;
//!   * every branch ran and none passed — a healthy search with the answer "no";
//!   * the gate could not judge anything, so nothing was spent at all.
//!
//! The third was an exit 0 until this module existed. `comp-goald` only asks
//! `status.success()`, so a REFUSED goal — its own check's command not in the
//! tree yet, or every check already green on the untouched base — was reported to
//! the platform as `awaiting-human`: a pull request waiting for review, when no
//! branch had run and nothing had been opened. Found live, not in theory.
//!
//! So the codes are named here, in one place both binaries read, and the human
//! sentence `comp-goald` sends to `/api/goals/{id}/fail` is derived from the code
//! rather than re-spelled at each call site.

/// The OS's own convention, and `comp-goald`'s `status.success()` depends on it
/// staying 0. A run that opened a pull request exits with this.
pub const SUCCESS: i32 = 0;

/// Every branch ran and the gate judged them, and none passed.
///
/// A legitimate outcome of a search, not a breakage — the difference matters
/// because one wants a better goal and the other wants someone to look at the
/// machine. A caller that wants to retry cares which, which is why this is not 1.
pub const EXHAUSTED: i32 = 3;

/// The gate could not judge anything, so nothing was spent.
///
/// Distinct from [`EXHAUSTED`] on purpose: no branch ran, no model was called,
/// no pull request exists. Reporting this as a finished run is what put a goal
/// nobody had written code for into the queue of goals waiting to be landed.
pub const GATE_REFUSED: i32 = 4;

/// What `comp-goalrun`'s exit code means, as a sentence for the platform's
/// `/api/goals/{id}/fail`.
///
/// `None` is a process killed by a signal — `std::process::ExitStatus::code()`
/// returns `None` there — and must not panic: this is the daemon reporting on
/// somebody else's outcome, and a crash here strands every other run in flight.
///
/// An unrecognised code is NAMED rather than hidden behind a generic phrase,
/// because an operator debugging a crash needs the number.
pub fn failure_reason(code: Option<i32>) -> String {
    match code {
        Some(EXHAUSTED) => {
            "no branch passed the gate — the goal needs work, not a retry".to_string()
        }
        Some(GATE_REFUSED) => "the gate refused to run: nothing was spent, no branch ran — the \
             goal's own checks cannot judge a candidate yet, so the fix is the goal, not the code"
            .to_string(),
        Some(SUCCESS) => "comp-goalrun exited 0 but nothing was opened — the harness is wrong, \
             not the goal"
            .to_string(),
        Some(code) => format!("comp-goalrun exited {code}"),
        None => "comp-goalrun was killed by a signal before it could report an outcome".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_distinct() {
        assert_ne!(SUCCESS, EXHAUSTED);
        assert_ne!(SUCCESS, GATE_REFUSED);
        assert_ne!(EXHAUSTED, GATE_REFUSED);
    }

    #[test]
    fn a_signal_kill_does_not_panic() {
        assert!(!failure_reason(None).is_empty());
    }

    #[test]
    fn an_unknown_code_names_itself() {
        assert!(failure_reason(Some(17)).contains("17"));
    }
}