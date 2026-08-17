//! `rrule` — expand a repeating or recurring event into concrete dates — every weekday, every 2 weeks on Mon and Wed
//!
//! Expand a normalized recurrence rule into occurrence dates (`YYYY-MM-DD`),
//! clipped to a half-open-ish inclusive window. Dates are days-since-epoch
//! internally (Hinnant civil<->days), so there is no date library and no
//! time-of-day — a recurrence is a set of DAYS and the caller owns the clock.
//!
//! Correctness points that are easy to get wrong and are handled here:
//!   * INTERVAL steps whole periods (N days for daily, N weeks for weekly).
//!   * BYDAY emits the listed weekdays *within* each weekly period, ascending.
//!   * COUNT is applied over the full series from `dtstart` — occurrences that
//!     fall before the window still consume the count — then results are
//!     clipped to `[from, to]`.
//!   * A hard cap (366) bounds unbounded (no COUNT / no UNTIL) rules.
//!
//! Pure compute — no state, no host imports.

#[allow(warnings)]
mod bindings;

use bindings::exports::rrule::recur::recur::{Freq, Guest, RecurError, Rule};

struct Component;

const CAP: usize = 366;

/// Days since the Unix epoch for a civil date (Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date from days since the Unix epoch (Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

/// ISO weekday, Monday = 0 … Sunday = 6.
fn weekday(days: i64) -> i64 {
    (((days % 7) + 3) % 7 + 7) % 7 // 1970-01-01 (day 0) was a Thursday
}

/// Parse `YYYY-MM-DD` to days-since-epoch.
fn parse(s: &str) -> Result<i64, RecurError> {
    let p: Vec<&str> = s.split('-').collect();
    let bad = || RecurError::BadDate(s.to_string());
    if p.len() != 3 {
        return Err(bad());
    }
    let y: i64 = p[0].parse().map_err(|_| bad())?;
    let m: i64 = p[1].parse().map_err(|_| bad())?;
    let d: i64 = p[2].parse().map_err(|_| bad())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    Ok(days_from_civil(y, m, d))
}

fn fmt(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn expand_impl(dtstart: &str, r: &Rule, from: &str, to: &str) -> Result<Vec<String>, RecurError> {
    let ds = parse(dtstart)?;
    let wf = parse(from)?;
    let wt = parse(to)?;
    let until = if r.until.is_empty() { None } else { Some(parse(&r.until)?) };
    let step = r.interval.max(1) as i64;
    let count = r.count as usize;

    let mut out: Vec<String> = Vec::new();
    let mut n = 0usize; // occurrences over the full series (for COUNT)

    // Stop conditions shared by both frequencies. `occ` is the candidate day.
    // Returns true if we should stop the whole expansion.
    let stop = |occ: i64, n: usize| -> bool {
        (count > 0 && n >= count) || until.map_or(false, |u| occ > u) || occ > wt || n > CAP
    };

    match r.frequency {
        Freq::Daily => {
            let mut day = ds;
            while !stop(day, n) {
                if day >= wf {
                    out.push(fmt(day));
                }
                n += 1;
                day += step;
            }
        }
        Freq::Weekly => {
            // weekdays within a week, ascending & de-duped; default = dtstart's.
            let mut wds: Vec<i64> = if r.by_weekday.is_empty() {
                vec![weekday(ds)]
            } else {
                let mut v: Vec<i64> = r.by_weekday.iter().map(|&w| w as i64).filter(|&w| (0..7).contains(&w)).collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            if wds.is_empty() {
                wds.push(weekday(ds));
            }
            let week0 = ds - weekday(ds); // Monday of dtstart's week
            let mut k: i64 = 0;
            'outer: loop {
                let base = week0 + k * 7 * step;
                if base > wt || (k as usize) > CAP {
                    break;
                }
                for &wd in &wds {
                    let occ = base + wd;
                    if occ < ds {
                        continue; // before the series start (first partial week)
                    }
                    if stop(occ, n) {
                        break 'outer;
                    }
                    if occ >= wf {
                        out.push(fmt(occ));
                    }
                    n += 1;
                }
                k += 1;
            }
        }
    }
    Ok(out)
}

impl Guest for Component {
    fn expand(dtstart: String, r: Rule, window_from: String, window_to: String) -> Result<Vec<String>, RecurError> {
        expand_impl(&dtstart, &r, &window_from, &window_to)
    }
}

bindings::export!(Component with_types_in bindings);
