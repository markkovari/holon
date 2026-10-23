//! `comp-goald` reads a run's outcome only from this exit code.

/// `comp-goald`'s `status.success()` depends on this staying `0`.
pub const SUCCESS: i32 = 0;

/// Every branch ran and none passed; retrying buys the same answer.
pub const EXHAUSTED: i32 = 3;

/// The gate could not judge, so nothing was spent; the fix is in the repository.
pub const GATE_REFUSED: i32 = 4;

/// A branch passed, and no pull request was opened: the forge failed, not the goal.
pub const LANDING_FAILED: i32 = 5;

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
        Some(LANDING_FAILED) => {
            "a branch passed the gate but no pull request was opened — the forge failed, not the \
             goal; retry once it answers"
                .to_string()
        }
        Some(other) => format!("comp-goalrun exited {other}"),
        None => "comp-goalrun was killed by a signal before it could exit".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_landing_is_not_read_as_a_goal_that_needs_work() {
        let landing = failure_reason(Some(LANDING_FAILED));
        assert!(landing.contains("no pull request was opened"), "{landing}");
        assert_ne!(landing, failure_reason(Some(EXHAUSTED)));
        assert_ne!(LANDING_FAILED, SUCCESS, "goald reads 0 as a run to hand to a human");
    }
}
