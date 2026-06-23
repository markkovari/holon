//! Minimal ISO-8601 -> unix-seconds parser for the appointment-datetime rules.
//!
//! The TS reference leans on the JS engine's `Date.parse`; a wasm component has
//! no such runtime, so this parses the practical subset the clinic uses —
//! `YYYY-MM-DD`, optionally `THH:MM[:SS]`, optionally a `Z` or `±HH:MM` offset.
//! Returns unix seconds (UTC). `None` for anything it can't read, which the
//! callers treat exactly like the TS `Number.isNaN(Date.parse(...))` path
//! (no reminder scheduled / `bad_datetime` on delete).

/// Parse an ISO-8601-ish datetime string to unix seconds (UTC). Best-effort:
/// a bare date is treated as midnight UTC; a missing offset is assumed UTC.
pub fn parse_unix_seconds(s: &str) -> Option<i64> {
    let s = s.trim();
    // split date and the rest (time + offset).
    let (date_part, rest) = match s.split_once(['T', ' ']) {
        Some((d, r)) => (d, r),
        None => (s, ""),
    };

    let mut dp = date_part.splitn(3, '-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // time + optional offset.
    let (time_part, offset_secs) = split_offset(rest);
    let (mut hour, mut min, mut sec) = (0i64, 0i64, 0i64);
    if !time_part.is_empty() {
        let mut tp = time_part.splitn(3, ':');
        hour = tp.next()?.parse().ok()?;
        min = tp.next().unwrap_or("0").parse().ok()?;
        // seconds may carry a fractional part (e.g. "30.500") — drop the frac.
        let sec_field = tp.next().unwrap_or("0");
        let sec_int = sec_field.split('.').next().unwrap_or("0");
        sec = sec_int.parse().ok()?;
    }
    if hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec;
    // an offset of +HH:MM means local time is ahead of UTC -> subtract to get UTC.
    Some(secs - offset_secs)
}

/// Split a `HH:MM:SS` + offset tail into (time, offset-seconds-from-UTC).
fn split_offset(rest: &str) -> (&str, i64) {
    if rest.is_empty() {
        return (rest, 0);
    }
    if let Some(stripped) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return (stripped, 0);
    }
    // find a +/- that introduces an offset (skip index 0 — never a sign there).
    if let Some(pos) = rest[1..].find(['+', '-']).map(|i| i + 1) {
        let (time, off) = rest.split_at(pos);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let mut op = off.splitn(2, ':');
        let oh: i64 = op.next().and_then(|h| h.parse().ok()).unwrap_or(0);
        let om: i64 = op.next().and_then(|m| m.parse().ok()).unwrap_or(0);
        return (time, sign * (oh * 3600 + om * 60));
    }
    (rest, 0)
}

/// Days since the unix epoch (1970-01-01) for a civil date. Howard Hinnant's
/// well-known branchless algorithm (valid for the Gregorian calendar).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(parse_unix_seconds("1970-01-01T00:00:00Z"), Some(0));
    }
    #[test]
    fn known_instant() {
        // 2021-01-01T00:00:00Z = 1609459200
        assert_eq!(parse_unix_seconds("2021-01-01T00:00:00Z"), Some(1_609_459_200));
    }
    #[test]
    fn bare_date() {
        assert_eq!(parse_unix_seconds("2021-01-01"), Some(1_609_459_200));
    }
    #[test]
    fn offset() {
        // 2021-01-01T01:00:00+01:00 == 2021-01-01T00:00:00Z
        assert_eq!(parse_unix_seconds("2021-01-01T01:00:00+01:00"), Some(1_609_459_200));
    }
    #[test]
    fn garbage() {
        assert_eq!(parse_unix_seconds("not-a-date"), None);
    }
}
