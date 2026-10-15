//! Is DeepSeek's off-peak discount active right now?
//!
//! Off-peak (docs.deepseek.com/quick_start/pricing) is 01:00-04:00 and 06:00-10:00 UTC on weekdays,
//! plus all 24 hours of weekends and Chinese public holidays.
//!
//! Pure integer arithmetic over a Unix timestamp, no date crate. The holiday calendar moves every
//! year, so callers supply it as a list of dates instead of a baked-in table that would go stale.

/// Whether `unix_secs` (UTC) falls in DeepSeek's off-peak window. `off_peak_days` is a set of Unix
/// DAY numbers (`unix_secs / 86400`) that are off-peak regardless, built from [`parse_holidays`].
pub fn deepseek_off_peak(unix_secs: u64, off_peak_days: &[i64]) -> bool {
    const SECS_PER_DAY: u64 = 86_400;
    let day = (unix_secs / SECS_PER_DAY) as i64;
    if off_peak_days.contains(&day) {
        return true;
    }
    if is_weekend(day) {
        return true;
    }
    let hour = (unix_secs % SECS_PER_DAY) / 3600;
    // Half-open: "01:00-04:00" means peak resumes AT 04:00, not after it.
    (1..4).contains(&hour) || (6..10).contains(&hour)
}

/// Saturday or Sunday, for a Unix day number.
///
/// Day 0 (1970-01-01) was a Thursday, so with Monday as 0 the weekday is `(day + 3) % 7`;
/// `rem_euclid` keeps it valid for negative days.
fn is_weekend(day: i64) -> bool {
    let weekday = (day + 3).rem_euclid(7);
    weekday == 5 || weekday == 6
}

/// Whether `model` is billed under DeepSeek's clock-dependent pricing (the same substring match
/// `cost_cents` uses). Every other model's price ignores the clock.
pub fn is_deepseek_model(model: &str) -> bool {
    model.contains("deepseek")
}

/// Pull a `--model <value>` argument out of a `comp-goalrun` argv slice. `None` if the flag is
/// absent or has nothing after it.
pub fn model_arg(argv: &[String]) -> Option<&str> {
    argv.iter().position(|a| a == "--model").and_then(|i| argv.get(i + 1)).map(String::as_str)
}

/// Whether a run using `model` should be held back right now, given whether off-peak enforcement is
/// turned on.
///
/// Enforcement is opt-in: a daemon nobody told to save money must not go silent for hours. A model
/// that is not DeepSeek's, or unknown (`None`), is never held back.
pub fn should_defer(now: u64, off_peak_days: &[i64], enforce: bool, model: Option<&str>) -> bool {
    if !enforce {
        return false;
    }
    let Some(model) = model else { return false };
    if !is_deepseek_model(model) {
        return false;
    }
    !deepseek_off_peak(now, off_peak_days)
}

/// Unix day number for a Gregorian civil date (UTC). Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (month as i64 + 9) % 12; // [0, 11], Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DD` lines into Unix day numbers. Blank lines and `#` lines are ignored.
pub fn parse_holidays(text: &str) -> Result<Vec<i64>, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|line| {
            let parts: Vec<&str> = line.split('-').collect();
            let [y, m, d] = parts.as_slice() else {
                return Err(format!("expected YYYY-MM-DD, got {line:?}"));
            };
            let y: i64 = y.parse().map_err(|_| format!("bad year in {line:?}"))?;
            let m: u32 = m.parse().map_err(|_| format!("bad month in {line:?}"))?;
            let d: u32 = d.parse().map_err(|_| format!("bad day in {line:?}"))?;
            if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
                return Err(format!("out-of-range month/day in {line:?}"));
            }
            Ok(days_from_civil(y, m, d))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// unix_secs for a UTC instant, built the way a caller would. Test-only.
    fn at(year: i64, month: u32, day: u32, hour: u64, minute: u64) -> u64 {
        (days_from_civil(year, month, day) as u64) * 86_400 + hour * 3600 + minute * 60
    }

    #[test]
    fn the_epoch_is_a_thursday_and_the_formula_knows_it() {
        assert!(!is_weekend(0));
        assert!(!is_weekend(1));
        assert!(is_weekend(2));
        assert!(is_weekend(3));
        assert!(!is_weekend(4));
    }

    #[test]
    fn days_from_civil_matches_known_reference_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 9, 19), 20715);
    }

    #[test]
    fn a_weekday_morning_window_is_off_peak() {
        // Tuesday 2026-09-15, 02:30 UTC: inside 01:00-04:00.
        assert!(deepseek_off_peak(at(2026, 9, 15, 2, 30), &[]));
        // Same day, 07:00 UTC — inside 06:00-10:00.
        assert!(deepseek_off_peak(at(2026, 9, 15, 7, 0), &[]));
    }

    #[test]
    fn a_weekday_outside_the_windows_is_peak() {
        // Tuesday 2026-09-15, 14:00 UTC — the middle of the business day.
        assert!(!deepseek_off_peak(at(2026, 9, 15, 14, 0), &[]));
        // 04:00 exactly — the window is half-open, peak resumes here.
        assert!(!deepseek_off_peak(at(2026, 9, 15, 4, 0), &[]));
        // 10:00 exactly — same for the second window.
        assert!(!deepseek_off_peak(at(2026, 9, 15, 10, 0), &[]));
    }

    #[test]
    fn a_whole_weekend_is_off_peak_any_hour() {
        // Saturday 2026-09-19, 14:00 UTC — the middle of the day, but a weekend.
        assert!(deepseek_off_peak(at(2026, 9, 19, 14, 0), &[]));
        // Sunday 2026-09-20, 23:00 UTC.
        assert!(deepseek_off_peak(at(2026, 9, 20, 23, 0), &[]));
    }

    #[test]
    fn a_listed_holiday_is_off_peak_even_on_a_weekday_at_noon() {
        let holiday = days_from_civil(2026, 10, 1); // a Thursday
        let noon = at(2026, 10, 1, 12, 0);
        assert!(!deepseek_off_peak(noon, &[]), "a plain Thursday noon is peak");
        assert!(deepseek_off_peak(noon, &[holiday]), "but the same day, listed, is off-peak");
    }

    #[test]
    fn parse_holidays_reads_dates_skips_blanks_and_comments() {
        let days = parse_holidays(
            "# China National Day 2026\n2026-10-01\n\n2026-10-02\n# note: golden week\n2026-10-07\n",
        )
        .unwrap();
        assert_eq!(
            days,
            vec![
                days_from_civil(2026, 10, 1),
                days_from_civil(2026, 10, 2),
                days_from_civil(2026, 10, 7)
            ]
        );
    }

    #[test]
    fn parse_holidays_rejects_a_malformed_line() {
        assert!(parse_holidays("2026-13-40").is_err(), "month 13 / day 40 must be refused");
        assert!(parse_holidays("not-a-date-at-all").is_err());
    }

    #[test]
    fn is_deepseek_model_matches_both_tiers_and_nothing_else() {
        assert!(is_deepseek_model("deepseek-flash"));
        assert!(is_deepseek_model("deepseek-v4-pro"));
        assert!(!is_deepseek_model("claude-haiku-4-5-20251001"));
        assert!(!is_deepseek_model("some-other-vendor/model"));
    }

    #[test]
    fn model_arg_finds_the_value_after_the_flag() {
        let argv: Vec<String> =
            ["--branches", "4", "--model", "deepseek-flash"].map(String::from).to_vec();
        assert_eq!(model_arg(&argv), Some("deepseek-flash"));
    }

    #[test]
    fn model_arg_is_none_when_the_flag_is_absent_or_trailing() {
        assert_eq!(model_arg(&["--branches".to_string(), "4".to_string()]), None);
        assert_eq!(model_arg(&["--model".to_string()]), None);
        assert_eq!(model_arg(&[]), None);
    }

    #[test]
    fn should_defer_is_false_when_enforcement_is_off_even_at_peak() {
        let peak = at(2026, 9, 15, 14, 0);
        assert!(!should_defer(peak, &[], false, Some("deepseek-flash")));
    }

    #[test]
    fn should_defer_ignores_a_non_deepseek_model_even_at_peak() {
        let peak = at(2026, 9, 15, 14, 0);
        assert!(!should_defer(peak, &[], true, Some("claude-haiku-4-5-20251001")));
    }

    #[test]
    fn should_defer_ignores_an_unknown_model() {
        let peak = at(2026, 9, 15, 14, 0);
        assert!(!should_defer(peak, &[], true, None));
    }

    #[test]
    fn should_defer_holds_a_deepseek_run_during_peak_hours() {
        let peak = at(2026, 9, 15, 14, 0);
        assert!(should_defer(peak, &[], true, Some("deepseek-flash")));
    }

    #[test]
    fn should_defer_releases_a_deepseek_run_once_off_peak() {
        let off = at(2026, 9, 15, 2, 0);
        assert!(!should_defer(off, &[], true, Some("deepseek-flash")));
        // A weekend releases it too, same as `deepseek_off_peak` itself.
        let weekend = at(2026, 9, 19, 14, 0);
        assert!(!should_defer(weekend, &[], true, Some("deepseek-v4-pro")));
    }
}
