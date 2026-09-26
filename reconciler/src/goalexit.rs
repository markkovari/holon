//! What `comp-goalrun`'s exit code MEANS, in one place.
//!
//! `comp-goald` reads a run's outcome off `ExitStatus` and nothing else — it
//! never sees stdout, and it cannot ask the child anything after it exits. So
//! the exit code is the whole report, and until this module every non-zero
//! outcome that was not a crash collapsed into one string. Worse, the two
//! outcomes that are most different — "every branch ran and none passed" and
//! "the gate refused to judge anything, nothing was spent" — were not
//! distinguishable at all, because a refused gate exited ZERO:
//!
//!     if !gate_can_judge(…) { return Ok(()); }
//!
//! That read as success to `status.success()`, so `comp-goald` moved the goal
//! to `awaiting-human` — a pull request waiting for review — when no branch had
//! run and nothing had been opened. Observed live, not theorised.
//!
//! Three codes, then, and the rule that keeps them honest: a code is only added
//! here when the CALLER must behave differently because of it.
//!
//!   - [`SUCCESS`] — a candidate passed and was landed.
//!   - [`EXHAUSTED`] — the search ran, and the answer was no. Retrying spends
//!     money for the same answer; a person has to change the goal.
//!   - [`GATE_REFUSED`] — the gate could not judge. NOTHING was spent, and the
//!     fix is a file in the repository, not a better model.
//!
//! Everything else is a crash or a harness fault, and the number itself is the
//! only useful thing to report — which is why the fallback names it.

/// A candidate passed the gate. The OS's own convention; `comp-goald`'s
/// `status.success()` check depends on this staying `0`.
pub const SUCCESS: i32 = 0;

/// Every branch ran, and none passed. A real search result: the harness worked
/// and the answer was no. Retrying buys the same answer.
pub const EXHAUSTED: i32 = 3;

/// The gate could not judge anything, so nothing was spent. Distinct from
/// [`EXHAUSTED`] because no branch ever ran, and distinct from `SUCCESS`
/// because nothing was opened — the two states this fix exists to separate.
pub const GATE_REFUSED: i32 = 4;

/// What `comp-goald` tells the platform's `/api/goals/{id}/fail` about a run
/// that did not succeed.
///
/// `code` is `ExitStatus::code()` as it comes: `None` means the process was
/// killed by a signal, which is a real thing a daemon reports on and must not
/// panic over.
///
/// The two named failures read DIFFERENTLY on purpose — a person reading
/// `/api/goals/{id}/fail` has to know whether to change the goal (exhausted) or
/// look at the repository (refused, and nothing was spent finding that out).
/// Anything unrecognised falls back to the numeric code, because an operator
/// debugging a crash needs the number and no wording can replace it.
pub fn failure_reason(code: Option<i32>) -> String {
    match code {
        // Same words this daemon printed before this module existed, kept
        // verbatim: the goal needs work, not a retry.
        Some(EXHAUSTED) => {
            "no branch passed the gate — the goal needs work, not a retry".to_string()
        }
        // "nothing was spent" is the load-bearing half. It is what tells the
        // reader not to treat this like a search result, and it is what the
        // held-out test asserts on.
        Some(GATE_REFUSED) => {
            "the gate could not judge anything — nothing was spent: no branch ran and no pull \
             request was opened"
                .to_string()
        }
        // A crash, a panic, a kill, or a code from a future version of the
        // runner. The number travels because it is the only lead there is.
        Some(other) => format!("comp-goalrun exited {other}"),
        None => "comp-goalrun was killed by a signal before it could exit".to_string(),
    }
}
