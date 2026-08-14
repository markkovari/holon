//! What a whole run has spent, in cents.
//!
//! `cost_cents` (in `cost.rs`) prices one completion. A run makes many, on
//! possibly different models once a tier router exists, so the run's spend is the
//! sum of its attempts' costs — the number a project budget is actually checked
//! against (goal 01 — fuel is money).
//!
//! UNIMPLEMENTED — this is the goal Holon is asked to fill in. The tests are the
//! specification.

use crate::cost::cost_cents;

/// One attempt's usage: input tokens, output tokens, and the model that answered.
pub type Usage<'a> = (u32, u32, &'a str);

/// The total cost, in whole cents, of every attempt in a run.
///
/// The sum of each attempt priced by `cost_cents` — not a re-derivation, so the
/// per-model pricing lives in exactly one place. An empty run has spent nothing.
pub fn spent_cents(attempts: &[Usage]) -> u64 {
    let _ = attempts;
    let _ = cost_cents(0, 0, "");
    unimplemented!("goal 01: sum each attempt's cost_cents — the tests below are the spec")
}

#[cfg(test)]
mod tests {
    use super::spent_cents;

    #[test]
    fn an_empty_run_has_spent_nothing() {
        assert_eq!(spent_cents(&[]), 0);
    }

    #[test]
    fn one_attempt_is_its_own_cost() {
        // 1M haiku input = 100 cents (see cost.rs).
        assert_eq!(spent_cents(&[(1_000_000, 0, "claude-haiku-4-5-20251001")]), 100);
    }

    #[test]
    fn attempts_sum() {
        // 100 (1M haiku input) + 500 (1M haiku output) = 600.
        assert_eq!(
            spent_cents(&[
                (1_000_000, 0, "claude-haiku-4-5-20251001"),
                (0, 1_000_000, "claude-haiku-4-5-20251001"),
            ]),
            600
        );
    }

    #[test]
    fn attempts_on_different_tiers_are_each_priced_at_their_own_rate() {
        // haiku 1M input (100) + opus 1M input (1500) = 1600.
        assert_eq!(
            spent_cents(&[
                (1_000_000, 0, "claude-haiku-4-5-20251001"),
                (1_000_000, 0, "claude-opus-5"),
            ]),
            1600
        );
    }
}
