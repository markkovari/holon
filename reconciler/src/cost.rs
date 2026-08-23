//! What a run costs, in cents.
//!
//! `spent-tokens` counts tokens; a project budget is money; and the two are in
//! different units nothing converts between (goal 01 — fuel is money). This is
//! that conversion, and it is deliberately a pure function: the prices are the
//! only thing that will ever be wrong, and a wrong price is a one-line fix with a
//! test beside it.
//!
//! The function below is UNIMPLEMENTED — it is the goal Holon is asked to fill
//! in. The tests are the specification.

/// The cost, in whole cents, of a completion that used `prompt_tokens` of input
/// and `completion_tokens` of output on `model`.
///
/// Prices are cents per MILLION tokens, input and output priced separately as
/// every provider prices them. A model the table does not know is charged at the
/// MOST EXPENSIVE tier, never free — a budget that treats an unknown model as
/// free is not a budget. The result rounds UP: underspending a cap is fine,
/// overspending it because of a floor is the failure this exists to prevent.
pub fn cost_cents(prompt_tokens: u32, completion_tokens: u32, model: &str) -> u64 {
    // Price table: (input cents per million, output cents per million)
    let (input_price, output_price) = if model.contains("haiku") {
        (100u64, 500u64)
    } else if model.contains("sonnet") {
        (300u64, 1500u64)
    } else if model.contains("opus") {
        (1500u64, 7500u64)
    } else {
        // Unknown model -> charge the most expensive tier (opus)
        (1500u64, 7500u64)
    };

    let prompt_cost = (prompt_tokens as u64 * input_price + 999_999) / 1_000_000;
    let completion_cost = (completion_tokens as u64 * output_price + 999_999) / 1_000_000;

    prompt_cost + completion_cost
}

#[cfg(test)]
mod tests {
    use super::cost_cents;

    // Prices, cents per million tokens (input, output), pinned by these tests:
    //   haiku   100 /  500
    //   sonnet  300 / 1500
    //   opus   1500 / 7500
    //   unknown -> opus (the most expensive known tier)
    // A model is matched by the tier name appearing in its id, e.g.
    // "claude-haiku-4-5-20251001" is haiku.

    #[test]
    fn a_million_input_tokens_of_haiku_is_its_input_price() {
        assert_eq!(cost_cents(1_000_000, 0, "claude-haiku-4-5-20251001"), 100);
    }

    #[test]
    fn a_million_output_tokens_of_haiku_is_its_output_price() {
        assert_eq!(cost_cents(0, 1_000_000, "claude-haiku-4-5-20251001"), 500);
    }

    #[test]
    fn input_and_output_are_summed_at_the_tier_price() {
        // sonnet: 300 input + 1500 output per million.
        assert_eq!(cost_cents(1_000_000, 1_000_000, "claude-sonnet-5"), 1800);
    }

    #[test]
    fn opus_is_the_dear_tier() {
        assert_eq!(cost_cents(1_000_000, 0, "claude-opus-5"), 1500);
    }

    #[test]
    fn an_unknown_model_is_charged_the_most_expensive_tier_not_free() {
        // Unknown -> opus input price, never 0.
        assert_eq!(cost_cents(1_000_000, 0, "some-other-vendor/model"), 1500);
    }

    #[test]
    fn a_tiny_usage_rounds_up_to_a_whole_cent_rather_than_down_to_zero() {
        // One haiku input token is 100/1_000_000 of a cent — rounds UP to 1.
        assert_eq!(cost_cents(1, 0, "claude-haiku-4-5-20251001"), 1);
    }

    #[test]
    fn zero_usage_is_zero() {
        assert_eq!(cost_cents(0, 0, "claude-haiku-4-5-20251001"), 0);
    }
}
