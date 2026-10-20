use crate::cost::cost_cents;

/// (input tokens, output tokens, model, ran in that model's off-peak window)
pub type Usage<'a> = (u32, u32, &'a str, bool);

pub fn spent_cents(attempts: &[Usage]) -> u64 {
    attempts
        .iter()
        .map(|(input, output, model, off_peak)| cost_cents(*input, *output, model, *off_peak))
        .sum()
}

/// A `cap_cents` of 0 means no cap.
pub fn over_budget(cap_cents: u64, attempts: &[Usage]) -> bool {
    if cap_cents == 0 {
        return false;
    }
    spent_cents(attempts) > cap_cents
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
        assert_eq!(spent_cents(&[(1_000_000, 0, "claude-haiku-4-5-20251001", false)]), 100);
    }

    #[test]
    fn attempts_sum() {
        assert_eq!(
            spent_cents(&[
                (1_000_000, 0, "claude-haiku-4-5-20251001", false),
                (0, 1_000_000, "claude-haiku-4-5-20251001", false),
            ]),
            600
        );
    }

    #[test]
    fn attempts_on_different_tiers_are_each_priced_at_their_own_rate() {
        assert_eq!(
            spent_cents(&[
                (1_000_000, 0, "claude-haiku-4-5-20251001", false),
                (1_000_000, 0, "claude-opus-5", false),
            ]),
            1600
        );
    }

    #[test]
    fn an_off_peak_deepseek_attempt_is_priced_cheaper_than_the_same_attempt_peak() {
        assert_eq!(spent_cents(&[(1_000_000, 0, "deepseek-flash", true)]), 15);
        assert_eq!(spent_cents(&[(1_000_000, 0, "deepseek-flash", false)]), 30);
    }

    #[test]
    fn a_zero_cap_means_no_cap_so_never_over() {
        assert!(!super::over_budget(0, &[(1_000_000, 1_000_000, "claude-opus-5", false)]));
    }

    #[test]
    fn spend_equal_to_the_cap_is_within_it() {
        assert!(!super::over_budget(100, &[(1_000_000, 0, "claude-haiku-4-5-20251001", false)]));
    }

    #[test]
    fn one_cent_over_the_cap_is_over() {
        assert!(super::over_budget(99, &[(1_000_000, 0, "claude-haiku-4-5-20251001", false)]));
    }

    #[test]
    fn an_empty_run_is_never_over_a_positive_cap() {
        assert!(!super::over_budget(1000, &[]));
    }
}
