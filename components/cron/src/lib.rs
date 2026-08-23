//! `cron` — parse a cron expression like `0 */6 * * *` and compute when it next fires
//!
//! Parses standard 5-field cron (`min hour dom month dow`) with `*`, `,`, `-`,
//! `/`, 3-letter month/day names, and the `@daily`/`@hourly`/… macros, all in
//! UTC. `matches` tests a timestamp; `next` returns upcoming fire times using a
//! day-at-a-time scan (skip whole non-matching days rather than every minute).
//!
//! Day-of-month vs day-of-week follow Vixie cron: if BOTH are restricted a day
//! matches when EITHER does; if one is `*` only the other constrains.
//!
//! Pure compute, no host imports, no state (the caller passes "now").

#[allow(warnings)]
mod bindings;

use bindings::exports::cron::expr::parser::{CronError, Guest};

struct Component;

const MONTHS: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];
const DAYS: &[(&str, u32)] =
    &[("sun", 0), ("mon", 1), ("tue", 2), ("wed", 3), ("thu", 4), ("fri", 5), ("sat", 6)];

struct Schedule {
    min: Vec<bool>,   // 0..=59
    hour: Vec<bool>,  // 0..=23
    dom: Vec<bool>,   // day 1..=31 -> index 0..=30
    month: Vec<bool>, // month 1..=12 -> index 0..=11
    dow: Vec<bool>,   // 0(Sun)..=6
    dom_star: bool,
    dow_star: bool,
}

fn inv(m: String) -> CronError {
    CronError::InvalidExpression(m)
}

/// Expand `@`-macros to a canonical 5-field string.
fn expand_macro(expr: &str) -> Option<&'static str> {
    match expr.trim() {
        "@yearly" | "@annually" => Some("0 0 1 1 *"),
        "@monthly" => Some("0 0 1 * *"),
        "@weekly" => Some("0 0 * * 0"),
        "@daily" | "@midnight" => Some("0 0 * * *"),
        "@hourly" => Some("0 * * * *"),
        _ => None,
    }
}

/// Resolve a single token (name or number) into a value, range-checked.
fn resolve(tok: &str, names: &[(&str, u32)], lo: u32, hi: u32) -> Result<u32, CronError> {
    let t = tok.trim().to_ascii_lowercase();
    if let Some((_, v)) = names.iter().find(|(n, _)| *n == t) {
        return Ok(*v);
    }
    let v: u32 = t.parse().map_err(|_| inv(format!("not a number or name: '{tok}'")))?;
    if v < lo || v > hi {
        return Err(inv(format!("{v} out of range {lo}-{hi}")));
    }
    Ok(v)
}

/// Parse one cron field into a bitset over `lo..=hi`, plus whether it was `*`.
fn parse_field(
    spec: &str,
    lo: u32,
    hi: u32,
    names: &[(&str, u32)],
) -> Result<(Vec<bool>, bool), CronError> {
    let size = (hi - lo + 1) as usize;
    let mut bits = vec![false; size];
    let star = spec.trim() == "*";
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(inv(format!("empty part in field '{spec}'")));
        }
        let (rng, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 =
                    s.trim().parse().map_err(|_| inv(format!("bad step '{s}' in '{part}'")))?;
                (r, step)
            }
            None => (part, 1),
        };
        if step == 0 {
            return Err(inv(format!("step cannot be 0 in '{part}'")));
        }
        let (start, end) = if rng == "*" {
            (lo, hi)
        } else if let Some((a, b)) = rng.split_once('-') {
            (resolve(a, names, lo, hi)?, resolve(b, names, lo, hi)?)
        } else {
            let v = resolve(rng, names, lo, hi)?;
            // "a/n" means a..=hi step n; a bare "a" is just a.
            if part.contains('/') {
                (v, hi)
            } else {
                (v, v)
            }
        };
        if start > end {
            return Err(inv(format!("range start > end in '{part}'")));
        }
        let mut v = start;
        while v <= end {
            bits[(v - lo) as usize] = true;
            v += step;
        }
    }
    Ok((bits, star))
}

fn build(expr: &str) -> Result<Schedule, CronError> {
    let expanded = expand_macro(expr).unwrap_or(expr);
    let fields: Vec<&str> = expanded.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(inv(format!(
            "expected 5 fields (min hour dom month dow), got {}",
            fields.len()
        )));
    }
    let (min, _) = parse_field(fields[0], 0, 59, &[])?;
    let (hour, _) = parse_field(fields[1], 0, 23, &[])?;
    let (dom, dom_star) = parse_field(fields[2], 1, 31, &[])?;
    let (month, _) = parse_field(fields[3], 1, 12, MONTHS)?;
    // day-of-week: accept 0..=7 (7 == Sunday), then fold 7 into 0.
    let (mut dow8, dow_star) = parse_field(fields[4], 0, 7, DAYS)?;
    if dow8[7] {
        dow8[0] = true;
    }
    dow8.truncate(7);
    Ok(Schedule { min, hour, dom, month, dow: dow8, dom_star, dow_star })
}

// ---- civil date math (UTC, days since 1970-01-01) -----------------------

/// (year, month 1-12, day 1-31) from days since the Unix epoch.
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Day of week, 0 = Sunday. 1970-01-01 was a Thursday (4).
fn weekday(z: i64) -> usize {
    ((z + 4).rem_euclid(7)) as usize
}

/// Whether the day (given its day-of-month and weekday) satisfies the schedule's
/// dom/dow fields, per Vixie cron's OR-when-both-restricted rule.
fn day_matches(s: &Schedule, dom: u32, wd: usize) -> bool {
    let dom_ok = s.dom[(dom - 1) as usize];
    let dow_ok = s.dow[wd];
    match (s.dom_star, s.dow_star) {
        (true, true) => true,
        (true, false) => dow_ok,
        (false, true) => dom_ok,
        (false, false) => dom_ok || dow_ok,
    }
}

fn matches_at(s: &Schedule, unix: u64) -> bool {
    let days = (unix / 86400) as i64;
    let sod = (unix % 86400) as usize;
    let minute = (sod / 60) % 60;
    let hour = sod / 3600;
    let (_, m, d) = civil_from_days(days);
    let wd = weekday(days);
    s.min[minute] && s.hour[hour] && s.month[(m - 1) as usize] && day_matches(s, d, wd)
}

// ---- normalize (for `parse`) --------------------------------------------

fn field_str(bits: &[bool], lo: u32) -> String {
    if bits.iter().all(|&b| b) {
        return "*".to_string();
    }
    bits.iter()
        .enumerate()
        .filter(|(_, &b)| b)
        .map(|(i, _)| (i as u32 + lo).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize(s: &Schedule) -> String {
    format!(
        "{} {} {} {} {}",
        field_str(&s.min, 0),
        field_str(&s.hour, 0),
        field_str(&s.dom, 1),
        field_str(&s.month, 1),
        field_str(&s.dow, 0),
    )
}

impl Guest for Component {
    fn parse(expr: String) -> Result<String, CronError> {
        Ok(normalize(&build(&expr)?))
    }

    fn matches(expr: String, unix: u64) -> Result<bool, CronError> {
        Ok(matches_at(&build(&expr)?, unix))
    }

    fn next(expr: String, after: u64, count: u32) -> Result<Vec<u64>, CronError> {
        let s = build(&expr)?;
        let count = count as usize;
        let mut out = Vec::new();
        if count == 0 {
            return Ok(out);
        }
        // Start at the next whole minute strictly after `after`.
        let mut t = (after / 60 + 1) * 60;
        // Scan day by day, skipping whole non-matching days. ~8-year horizon.
        for _ in 0..(8 * 366) {
            let day = (t / 86400) as i64;
            let day_start = (day as u64) * 86400;
            let (_, m, d) = civil_from_days(day);
            let wd = weekday(day);
            if s.month[(m - 1) as usize] && day_matches(&s, d, wd) {
                let start_slot = ((t - day_start) / 60) as usize; // 0..=1439
                for slot in start_slot..1440 {
                    if s.hour[slot / 60] && s.min[slot % 60] {
                        out.push(day_start + (slot as u64) * 60);
                        if out.len() == count {
                            return Ok(out);
                        }
                    }
                }
            }
            t = day_start + 86400; // jump to the next day's 00:00
        }
        if out.is_empty() {
            return Err(CronError::Unsatisfiable(format!(
                "'{expr}' does not fire within the 8-year horizon"
            )));
        }
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn next(expr: &str, after: u64, n: u32) -> Vec<u64> {
        <Component as Guest>::next(expr.into(), after, n).unwrap()
    }

    #[test]
    fn civil_roundtrip_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday(0), 4); // Thursday
                                   // 2021-01-01 was a Friday; days = 18628
        assert_eq!(civil_from_days(18628), (2021, 1, 1));
        assert_eq!(weekday(18628), 5);
    }

    #[test]
    fn parse_normalizes_macros_and_names() {
        assert_eq!(<Component as Guest>::parse("@hourly".into()).unwrap(), "0 * * * *");
        assert_eq!(<Component as Guest>::parse("@daily".into()).unwrap(), "0 0 * * *");
        // names + step lower to numbers
        assert_eq!(<Component as Guest>::parse("0 0 * jan mon".into()).unwrap(), "0 0 * 1 1");
        assert_eq!(
            <Component as Guest>::parse("*/15 * * * *".into()).unwrap(),
            "0,15,30,45 * * * *"
        );
    }

    #[test]
    fn every_six_hours() {
        // "0 */6 * * *" from 2021-01-01 00:00:00 UTC (t=1609459200)
        let t0 = 1609459200;
        let got = next("0 */6 * * *", t0, 4);
        assert_eq!(got, vec![t0 + 6 * 3600, t0 + 12 * 3600, t0 + 18 * 3600, t0 + 24 * 3600]);
    }

    #[test]
    fn matches_and_dom_dow_or_rule() {
        let s = build("30 9 * * mon").unwrap();
        // 2021-01-04 09:30 UTC was a Monday
        let mon_0930 = 1609752600;
        assert!(matches_at(&s, mon_0930));
        assert!(!matches_at(&s, mon_0930 + 86400)); // Tuesday
                                                    // both dom and dow restricted -> OR: fires on the 1st OR any Friday
        let s2 = build("0 0 1 * fri").unwrap();
        assert!(!s2.dom_star && !s2.dow_star);
    }

    #[test]
    fn leap_day_yearly() {
        // "0 0 29 2 *" — next Feb 29 at midnight after 2021-01-01 is 2024-02-29.
        let got = next("0 0 29 2 *", 1609459200, 1);
        assert_eq!(got, vec![1709164800]); // 2024-02-29 00:00:00 UTC
    }

    #[test]
    fn errors() {
        assert!(<Component as Guest>::parse("* * *".into()).is_err()); // too few fields
        assert!(<Component as Guest>::parse("60 * * * *".into()).is_err()); // minute 60
        assert!(<Component as Guest>::parse("* * * * xyz".into()).is_err()); // bad name
        assert!(<Component as Guest>::parse("*/0 * * * *".into()).is_err()); // zero step
    }
}
