//! What a run costs, in cents.
//!
//! `spent-tokens` counts tokens; a project budget is money; and the two are in
//! different units nothing converts between (goal 01 — fuel is money). This is
//! that conversion, and it is deliberately a pure function: the prices are the
//! only thing that will ever be wrong, and a wrong price is a one-line fix with a
//! test beside it.
//!
//! Holon filled `cost_cents` in against the tests below, which are its spec.
//! `cost_usd_micros` is the same table at the resolution one model call needs:
//! a single haiku call is a fraction of a cent, and a session budget summed
//! from per-call cents would overcharge every call by up to a cent.

/// The cost, in whole cents, of a completion that used `prompt_tokens` of input
/// and `completion_tokens` of output on `model`.
///
/// Prices are cents per MILLION tokens, input and output priced separately as
/// every provider prices them. A model the table does not know is charged at the
/// MOST EXPENSIVE tier, never free — a budget that treats an unknown model as
/// free is not a budget. The result rounds UP: underspending a cap is fine,
/// overspending it because of a floor is the failure this exists to prevent.
///
/// `off_peak` only changes the price of a DeepSeek model — see
/// [`offpeak::deepseek_off_peak`](crate::offpeak::deepseek_off_peak) for what
/// counts as off-peak. Every other model ignores it; DeepSeek is the only
/// provider in this table whose price is a function of the clock.
pub fn cost_cents(prompt_tokens: u32, completion_tokens: u32, model: &str, off_peak: bool) -> u64 {
    let (input_price, output_price) = prices(model, off_peak);
    let prompt_cost = (prompt_tokens as u64 * input_price).div_ceil(1_000_000);
    let completion_cost = (completion_tokens as u64 * output_price).div_ceil(1_000_000);
    prompt_cost + completion_cost
}

/// The cost, in micro-USD (1 USD = 1_000_000), of one call's tokens.
///
/// Same table and the same rules as [`cost_cents`] — unknown is the dearest
/// tier, rounding is UP — plus prompt caching, which every provider here
/// prices off the input rate: a cache read is 10% of it, a cache write 125%.
/// Rounded once over the sum, so four small counts do not round up four times.
pub fn cost_usd_micros(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    model: &str,
    off_peak: bool,
) -> u64 {
    let (i, o) = prices(model, off_peak);
    micros_at(i, o, input, output, cache_read, cache_write)
}

/// [`cost_usd_micros`] at explicit prices (cents per million tokens, input
/// and output), for a model the table cannot know — a self-hosted one an
/// operator prices at zero, say.
pub fn micros_at(
    i: u64,
    o: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> u64 {
    // cents/M tokens -> micro-USD is x10_000 / 1_000_000, i.e. /100; the cache
    // multipliers (0.1, 1.25) are folded in as /10_000 with integer weights.
    let scaled = (input * i + output * o) * 100 + cache_read * i * 10 + cache_write * i * 125;
    scaled.div_ceil(10_000)
}

/// (input, output) cents per million tokens for `model`.
fn prices(model: &str, off_peak: bool) -> (u64, u64) {
    if model.contains("deepseek-flash") {
        if off_peak {
            (15, 60)
        } else {
            (30, 120)
        }
    } else if model.contains("deepseek") {
        if off_peak {
            (66, 198)
        } else {
            (132, 396)
        }
    } else if model.contains("haiku") {
        (100, 500)
    } else if model.contains("sonnet") {
        (300, 1500)
    } else {
        // opus, and anything unknown: the most expensive tier, never free.
        (1500, 7500)
    }
}

#[cfg(test)]
mod tests {
    use super::{cost_cents, cost_usd_micros, micros_at};

    // Prices, cents per million tokens (input, output), pinned by these tests:
    //   haiku            100 /  500
    //   sonnet           300 / 1500
    //   opus            1500 / 7500
    //   deepseek-flash off-peak  15 /   60, peak  30 /  120
    //   deepseek (v4-pro) off-peak 66 / 198, peak 132 / 396
    //   unknown -> opus (the most expensive known tier)
    // A model is matched by the tier name appearing in its id, e.g.
    // "claude-haiku-4-5-20251001" is haiku. `off_peak` only affects deepseek.

    #[test]
    fn a_million_input_tokens_of_haiku_is_its_input_price() {
        assert_eq!(cost_cents(1_000_000, 0, "claude-haiku-4-5-20251001", false), 100);
    }

    #[test]
    fn a_million_output_tokens_of_haiku_is_its_output_price() {
        assert_eq!(cost_cents(0, 1_000_000, "claude-haiku-4-5-20251001", false), 500);
    }

    #[test]
    fn input_and_output_are_summed_at_the_tier_price() {
        // sonnet: 300 input + 1500 output per million.
        assert_eq!(cost_cents(1_000_000, 1_000_000, "claude-sonnet-5", false), 1800);
    }

    #[test]
    fn opus_is_the_dear_tier() {
        assert_eq!(cost_cents(1_000_000, 0, "claude-opus-5", false), 1500);
    }

    #[test]
    fn an_unknown_model_is_charged_the_most_expensive_tier_not_free() {
        // Unknown -> opus input price, never 0.
        assert_eq!(cost_cents(1_000_000, 0, "some-other-vendor/model", false), 1500);
    }

    #[test]
    fn a_tiny_usage_rounds_up_to_a_whole_cent_rather_than_down_to_zero() {
        // One haiku input token is 100/1_000_000 of a cent — rounds UP to 1.
        assert_eq!(cost_cents(1, 0, "claude-haiku-4-5-20251001", false), 1);
    }

    #[test]
    fn zero_usage_is_zero() {
        assert_eq!(cost_cents(0, 0, "claude-haiku-4-5-20251001", false), 0);
    }

    #[test]
    fn deepseek_flash_is_half_price_off_peak() {
        assert_eq!(cost_cents(1_000_000, 1_000_000, "deepseek-flash", true), 75);
        assert_eq!(cost_cents(1_000_000, 1_000_000, "deepseek-flash", false), 150);
    }

    #[test]
    fn deepseek_v4_pro_is_half_price_off_peak() {
        assert_eq!(cost_cents(1_000_000, 1_000_000, "deepseek-v4-pro", true), 264);
        assert_eq!(cost_cents(1_000_000, 1_000_000, "deepseek-v4-pro", false), 528);
    }

    #[test]
    fn deepseek_flash_is_cheaper_than_haiku_even_at_peak() {
        // The whole point of the low-budget path: flash beats haiku even
        // without the off-peak discount.
        assert!(
            cost_cents(1_000_000, 0, "deepseek-flash", false)
                < cost_cents(1_000_000, 0, "claude-haiku-4-5-20251001", false)
        );
    }

    #[test]
    fn micros_agree_with_cents_on_whole_cents() {
        // 1M sonnet in + 1M out = 1800 cents = $18.
        assert_eq!(
            cost_usd_micros(1_000_000, 1_000_000, 0, 0, "claude-sonnet-5", false),
            18_000_000
        );
    }

    #[test]
    fn one_haiku_input_token_is_one_micro_not_a_whole_cent() {
        // 100 cents/M = $1/M = 1 micro-USD per token.
        assert_eq!(cost_usd_micros(1, 0, 0, 0, "claude-haiku-4-5-20251001", false), 1);
        assert_eq!(cost_usd_micros(0, 0, 0, 0, "claude-haiku-4-5-20251001", false), 0);
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input_and_writes_a_quarter_more() {
        // sonnet input $3/M: 1M cache reads = $0.30, 1M cache writes = $3.75.
        assert_eq!(cost_usd_micros(0, 0, 1_000_000, 0, "claude-sonnet-5", false), 300_000);
        assert_eq!(cost_usd_micros(0, 0, 0, 1_000_000, "claude-sonnet-5", false), 3_750_000);
    }

    #[test]
    fn micros_round_up_once_over_the_sum() {
        // Each count alone is 0.1 micro (sonnet cache read of one token is
        // 0.03, input 3): one read + one input = 3.03 -> 4, not 3 + 1 + ...
        assert_eq!(cost_usd_micros(1, 0, 1, 0, "claude-sonnet-5", false), 4);
    }

    #[test]
    fn micros_charge_an_unknown_model_as_opus() {
        assert_eq!(
            cost_usd_micros(1000, 0, 0, 0, "mlx-community/Qwen2.5-Coder-32B", false),
            cost_usd_micros(1000, 0, 0, 0, "claude-opus-5", false)
        );
    }

    #[test]
    fn explicit_prices_override_the_table_and_zero_is_free() {
        assert_eq!(micros_at(0, 0, 1_000_000, 1_000_000, 5, 5), 0);
        assert_eq!(
            micros_at(300, 1500, 7, 3, 2, 1),
            cost_usd_micros(7, 3, 2, 1, "claude-sonnet-5", false)
        );
    }
}
