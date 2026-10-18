//! What `comp-goalrun`'s exit code MEANS, in one place.
//!
//! `comp-goald` reads a run's outcome off `ExitStatus` and nothing else (never stdout), so the exit code
//! is the whole report. A refused gate once exited ZERO (`if !gate_can_judge(…) { return Ok(()); }`),
//! so `status.success()` moved the goal to `awaiting-human`, a PR waiting for review, when no branch
//! had run and nothing was opened.
//!
//! A code is only added here when the CALLER must behave differently because of it:
//!
//! - [`SUCCESS`] — a candidate passed and was landed.
//! - [`EXHAUSTED`] — the search ran, and the answer was no. Retrying spends money for the same
//! answer; a person has to change the goal.
//! - [`GATE_REFUSED`] — the gate could not judge. NOTHING was spent, and the fix is a file in the
//! repository, not a better model.
//!
//! Everything else is a crash or a harness fault, and the number itself is the only useful thing to
//! report, which is why the fallback names it.

/// A candidate passed the gate. The OS's own convention; `comp-goald`'s `status.success()` check
/// depends on this staying `0`.
pub const SUCCESS: i32 = 0;

/// Every branch ran, and none passed. A real search result; retrying buys the same answer.
pub const EXHAUSTED: i32 = 3;

/// The gate could not judge anything, so nothing was spent. Distinct from [`EXHAUSTED`] (no branch
/// ever ran) and from `SUCCESS` (nothing was opened).
pub const GATE_REFUSED: i32 = 4;

/// What `comp-goald` tells the platform's `/api/goals/{id}/fail` about a run that did not succeed.
///
/// `code` is `ExitStatus::code()` as it comes: `None` means the process was killed by a signal, which
/// a daemon must report and not panic over.
///
/// The two named failures read DIFFERENTLY on purpose: a person reading the fail report has to know
/// whether to change the goal (exhausted) or look at the repository (refused). Anything unrecognised
/// falls back to the numeric code, which no wording can replace when debugging a crash.
pub fn failure_reason(code: Option<i32>) -> String {
    match code {
        // Kept verbatim from before this module existed: the goal needs work, not a retry.
        Some(EXHAUSTED) => {
            "no branch passed the gate — the goal needs work, not a retry".to_string()
        }
        // "nothing was spent" is the load-bearing half: it tells the reader not to treat this as a search
        // result, and the held-out test asserts on it.
        Some(GATE_REFUSED) => {
            "the gate could not judge anything — nothing was spent: no branch ran and no pull \
             request was opened"
                .to_string()
        }
        // A crash, a panic, a kill, or a code from a future runner: the number is the only lead.
        Some(other) => format!("comp-goalrun exited {other}"),
        None => "comp-goalrun was killed by a signal before it could exit".to_string(),
    }
}
