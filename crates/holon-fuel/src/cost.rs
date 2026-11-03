/// Rounds UP: overspending a cap because of a floor is the failure this exists to prevent.
pub fn cost_cents(prompt_tokens: u32, completion_tokens: u32, model: &str, off_peak: bool) -> u64 {
    let (input_price, output_price) = prices(model, off_peak);
    let prompt_cost = (prompt_tokens as u64 * input_price).div_ceil(1_000_000);
    let completion_cost = (completion_tokens as u64 * output_price).div_ceil(1_000_000);
    prompt_cost + completion_cost
}

/// Micro-USD: per-call cents would overcharge each call by up to a cent. Rounded once over the sum.
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

pub fn micros_at(
    i: u64,
    o: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> u64 {
    // cents/M tokens -> micro-USD is /100; cache weights 0.1 and 1.25 folded in as /10_000.
    let scaled = (input * i + output * o) * 100 + cache_read * i * 10 + cache_write * i * 125;
    scaled.div_ceil(10_000)
}

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
        // Unknown models pay the most expensive tier, never free.
        (1500, 7500)
    }
}

#[cfg(test)]
mod tests {
    use super::{cost_cents, cost_usd_micros, micros_at};

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
        assert_eq!(cost_cents(1_000_000, 1_000_000, "claude-sonnet-5", false), 1800);
    }

    #[test]
    fn opus_is_the_dear_tier() {
        assert_eq!(cost_cents(1_000_000, 0, "claude-opus-5", false), 1500);
    }

    #[test]
    fn an_unknown_model_is_charged_the_most_expensive_tier_not_free() {
        assert_eq!(cost_cents(1_000_000, 0, "some-other-vendor/model", false), 1500);
    }

    #[test]
    fn a_tiny_usage_rounds_up_to_a_whole_cent_rather_than_down_to_zero() {
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
        // The point of the low-budget path: flash beats haiku even without the off-peak discount.
        assert!(
            cost_cents(1_000_000, 0, "deepseek-flash", false)
                < cost_cents(1_000_000, 0, "claude-haiku-4-5-20251001", false)
        );
    }

    #[test]
    fn micros_agree_with_cents_on_whole_cents() {
        assert_eq!(
            cost_usd_micros(1_000_000, 1_000_000, 0, 0, "claude-sonnet-5", false),
            18_000_000
        );
    }

    #[test]
    fn one_haiku_input_token_is_one_micro_not_a_whole_cent() {
        assert_eq!(cost_usd_micros(1, 0, 0, 0, "claude-haiku-4-5-20251001", false), 1);
        assert_eq!(cost_usd_micros(0, 0, 0, 0, "claude-haiku-4-5-20251001", false), 0);
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input_and_writes_a_quarter_more() {
        assert_eq!(cost_usd_micros(0, 0, 1_000_000, 0, "claude-sonnet-5", false), 300_000);
        assert_eq!(cost_usd_micros(0, 0, 0, 1_000_000, "claude-sonnet-5", false), 3_750_000);
    }

    #[test]
    fn micros_round_up_once_over_the_sum() {
        // 0.03 + 3 = 3.03 rounds once to 4.
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
