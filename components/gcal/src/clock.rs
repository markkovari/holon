//! Local dates, from `agent-wire`'s clock (a component cannot depend on a native workspace crate).

/// Howard Hinnant's civil_from_days.
fn ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Howard Hinnant's days_from_civil.
fn days_from_ymd(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD[THH:MM]` as (days since the epoch, minute of the day).
pub fn parse_local(s: &str) -> Option<(i64, u32)> {
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut parts = date.split('-');
    let (y, m, d) = (
        parts.next()?.parse::<i64>().ok()?,
        parts.next()?.parse::<u32>().ok()?,
        parts.next()?.parse::<u32>().ok()?,
    );
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_ymd(y, m, d);
    if ymd(days) != (y, m, d) {
        return None; // 2026-02-30 rolls into March: not a date
    }
    let minute = match time {
        None => 0,
        Some(t) => {
            let (h, mi) = t.split_once(':')?;
            let (h, mi) = (h.parse::<u32>().ok()?, mi.get(..2)?.parse::<u32>().ok()?);
            if h > 23 || mi > 59 || t.len() > 8 {
                return None;
            }
            h * 60 + mi
        }
    };
    Some((days, minute))
}

pub fn date_of(days: i64) -> String {
    let (y, m, d) = ymd(days);
    format!("{y}-{m:02}-{d:02}")
}

pub fn local_of(days: i64, minute: i64) -> String {
    let days = days + minute.div_euclid(1440);
    let minute = minute.rem_euclid(1440);
    format!("{}T{:02}:{:02}", date_of(days), minute / 60, minute % 60)
}
