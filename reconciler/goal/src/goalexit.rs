//! `comp-goald` reads a run's outcome only from this exit code.

/// `comp-goald`'s `status.success()` depends on this staying `0`.
pub const SUCCESS: i32 = 0;

/// Every branch ran and none passed; retrying buys the same answer.
pub const EXHAUSTED: i32 = 3;

/// The gate could not judge, so nothing was spent; the fix is in the repository.
pub const GATE_REFUSED: i32 = 4;

/// `None` is a signal kill. The named failures read differently: change the goal vs. fix the repo.
pub fn failure_reason(code: Option<i32>) -> String {
    match code {
        Some(EXHAUSTED) => {
            "no branch passed the gate — the goal needs work, not a retry".to_string()
        }
        // "nothing was spent" is load-bearing; a held-out test asserts on it.
        Some(GATE_REFUSED) => {
            "the gate could not judge anything — nothing was spent: no branch ran and no pull \
             request was opened"
                .to_string()
        }
        Some(other) => format!("comp-goalrun exited {other}"),
        None => "comp-goalrun was killed by a signal before it could exit".to_string(),
    }
}
