//! `rrule` — expand a repeating or recurring event into concrete dates — every weekday, every 2 weeks on Mon and Wed
//! before clipping; a hard cap (366) bounds unbounded rules.

#[allow(warnings)]
mod bindings;

use bindings::exports::rrule::recur::recur::{Freq, Guest, RecurError, Rule};

mod dates;
use dates::{civil_from_days, days_from_civil, weekday};

struct Component;

const CAP: usize = 366;

fn parse(s: &str) -> Result<i64, RecurError> {
    let p: Vec<&str> = s.split('-').collect();
    let bad = || RecurError::BadDate(s.to_string());
    if p.len() != 3 {
        return Err(bad());
    }
    let y: i64 = p[0].parse().map_err(|_| bad())?;
    let m: i64 = p[1].parse().map_err(|_| bad())?;
    let d: i64 = p[2].parse().map_err(|_| bad())?;
    // Bounding the year: unbounded digits overflow `era * 146097` in `days_from_civil` (fuzz-found).
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(-9999..=9999).contains(&y) {
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

    let stop = |occ: i64, n: usize| -> bool {
        (count > 0 && n >= count) || until.is_some_and(|u| occ > u) || occ > wt || n > CAP
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
            let mut wds: Vec<i64> = if r.by_weekday.is_empty() {
                vec![weekday(ds)]
            } else {
                let mut v: Vec<i64> = r
                    .by_weekday
                    .iter()
                    .map(|&w| w as i64)
                    .filter(|&w| (0..7).contains(&w))
                    .collect();
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
    fn expand(
        dtstart: String,
        r: Rule,
        window_from: String,
        window_to: String,
    ) -> Result<Vec<String>, RecurError> {
        expand_impl(&dtstart, &r, &window_from, &window_to)
    }
}

bindings::export!(Component with_types_in bindings);
