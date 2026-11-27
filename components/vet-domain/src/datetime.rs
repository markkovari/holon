//! The TS reference leans on `Date.parse`, which a wasm component lacks; `None` is its `NaN` path.

/// A bare date is midnight UTC; a missing offset is UTC.
pub fn parse_unix_seconds(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date_part, rest) = match s.split_once(['T', ' ']) {
        Some((d, r)) => (d, r),
        None => (s, ""),
    };

    let mut dp = date_part.splitn(3, '-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    // Untrusted, unbounded digits would overflow `era * 146_097` in `days_from_civil`.
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(-9999..=9999).contains(&year) {
        return None;
    }

    let (time_part, offset_secs) = split_offset(rest)?;
    let (mut hour, mut min, mut sec) = (0i64, 0i64, 0i64);
    if !time_part.is_empty() {
        let mut tp = time_part.splitn(3, ':');
        hour = tp.next()?.parse().ok()?;
        min = tp.next().unwrap_or("0").parse().ok()?;
        let sec_field = tp.next().unwrap_or("0");
        let sec_int = sec_field.split('.').next().unwrap_or("0");
        sec = sec_int.parse().ok()?;
    }
    // Lower-bounded too: a huge negative i64 would pass `> 23` and overflow `hour * 3600`.
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) || !(0..=60).contains(&sec) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec;
    Some(secs - offset_secs)
}

/// `None` for an out-of-range offset, which would otherwise overflow `oh * 3600`.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if rest.is_empty() {
        return Some((rest, 0));
    }
    if let Some(stripped) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return Some((stripped, 0));
    }
    // Skip the first char by char, not byte: a multi-byte first char would panic a raw `[1..]`.
    let skip_first = rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i);
    if let Some(pos) = rest[skip_first..].find(['+', '-']).map(|i| i + skip_first) {
        let (time, off) = rest.split_at(pos);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let mut op = off.splitn(2, ':');
        let oh: i64 = op.next().and_then(|h| h.parse().ok()).unwrap_or(0);
        let om: i64 = op.next().and_then(|m| m.parse().ok()).unwrap_or(0);
        if !(0..=23).contains(&oh) || !(0..=59).contains(&om) {
            return None;
        }
        return Some((time, sign * (oh * 3600 + om * 60)));
    }
    Some((rest, 0))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: Unix seconds -> `YYYY-MM-DD` (UTC).
pub fn format_date(unix_secs: i64) -> String {
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Add `days` to a `YYYY-MM-DD` date string.
pub fn add_days(date: &str, days: i64) -> Option<String> {
    let secs = parse_unix_seconds(date)?;
    Some(format_date(secs + days * 86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(parse_unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(format_date(0), "1970-01-01");
    }
    #[test]
    fn known_instant() {
        assert_eq!(parse_unix_seconds("2021-01-01T00:00:00Z"), Some(1_609_459_200));
        assert_eq!(format_date(1_609_459_200), "2021-01-01");
    }
    #[test]
    fn bare_date() {
        assert_eq!(parse_unix_seconds("2021-01-01"), Some(1_609_459_200));
        assert_eq!(add_days("2026-11-27", 365), Some("2027-11-27".to_string()));
    }
    #[test]
    fn offset() {
        assert_eq!(parse_unix_seconds("2021-01-01T01:00:00+01:00"), Some(1_609_459_200));
    }
    #[test]
    fn garbage() {
        assert_eq!(parse_unix_seconds("not-a-date"), None);
    }
}
