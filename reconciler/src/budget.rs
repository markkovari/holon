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
    attempts
        .iter()
        .map(|(input, output, model)| cost_cents(*input, *output, model))
        .sum()
}

/// Has this run spent past its cap? `cap_cents` of 0 means no cap, so a run with
/// no budget is never over one — matching the codebase's "0 is unbounded"
/// convention (a cap of zero is the absence of a cap, not a cap of nothing).
/// Equal to the cap is within it; only strictly more is over.
pub fn over_budget(cap_cents: u64, attempts: &[Usage]) -> bool {
    let _ = (cap_cents, attempts);
    unimplemented!("goal 01: is the run over its cap? — the tests below are the spec")
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

    #[test]
    fn a_zero_cap_means_no_cap_so_never_over() {
        assert_eq!(super::over_budget(0, &[(1_000_000, 1_000_000, "claude-opus-5")]), false);
    }

    #[test]
    fn spend_equal_to_the_cap_is_within_it() {
        // 1M haiku input = 100 cents; cap 100 is not exceeded.
        assert_eq!(super::over_budget(100, &[(1_000_000, 0, "claude-haiku-4-5-20251001")]), false);
    }

    #[test]
    fn one_cent_over_the_cap_is_over() {
        assert_eq!(super::over_budget(99, &[(1_000_000, 0, "claude-haiku-4-5-20251001")]), true);
    }

    #[test]
    fn an_empty_run_is_never_over_a_positive_cap() {
        assert_eq!(super::over_budget(1000, &[]), false);
    }
}