//! What the `now` tool says. A bare unix number is a poor answer for a model that has to
//! plan "tomorrow" or "next Friday": it converted one wrongly and a calendar event landed a
//! day late. So the answer is the local date in words, with the days ahead spelled out, and
//! UTC and unix time kept for anything that wants them.

const DAYS: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// (year, month, day) of a day count since 1970-01-01 (Howard Hinnant's civil_from_days).
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

fn weekday(days: i64) -> &'static str {
    DAYS[(days + 4).rem_euclid(7) as usize] // 1970-01-01 was a Thursday
}

fn date(days: i64) -> String {
    let (y, m, d) = ymd(days);
    format!("{} {y}-{m:02}-{d:02}", weekday(days))
}

fn offset(secs: i64) -> String {
    let (sign, a) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
    format!("UTC{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

/// The text of `now`. `local` gives the machine's UTC offset in seconds and its zone name
/// at a unix time (a parameter so a test can pin it).
pub fn describe(unix: u64, local: &dyn Fn(u64) -> (i64, String)) -> String {
    let (off, zone) = local(unix);
    let lt = unix as i64 + off;
    let (days, secs) = (lt.div_euclid(86_400), lt.rem_euclid(86_400));
    let utc = unix as i64;
    let (udays, usecs) = (utc.div_euclid(86_400), utc.rem_euclid(86_400));
    let later: Vec<String> = (2..=7).map(|n| date(days + n)).collect();
    format!(
        "Now it is {} {:02}:{:02} {zone} ({}). UTC: {} {:02}:{:02}. Unix: {unix}.\nTomorrow is {}. After that: {}.",
        date(days),
        secs / 3600,
        secs % 3600 / 60,
        offset(off),
        date(udays),
        usecs / 3600,
        usecs % 3600 / 60,
        date(days + 1),
        later.join(", ")
    )
}

/// This machine's current UTC offset (seconds) and zone name, from the system `date`: the
/// one place that already knows the user's time zone, and it needs no `unsafe` or C library.
/// `now` is only ever asked about the present, so the argument is not used; a failure falls
/// back to UTC.
pub fn local_offset(_unix: u64) -> (i64, String) {
    let out = std::process::Command::new("date").arg("+%z %Z").output();
    let text = out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    text.as_deref().and_then(parse_offset).unwrap_or((0, "UTC".to_string()))
}

/// `"+0200 CEST"` -> (7200, "CEST").
fn parse_offset(s: &str) -> Option<(i64, String)> {
    let (z, name) = s.split_once(' ')?;
    let sign = match z.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (h, m) = (z.get(1..3)?.parse::<i64>().ok()?, z.get(3..5)?.parse::<i64>().ok()?);
    Some((sign * (h * 3600 + m * 60), name.to_string()))
}

pub fn now(unix: u64) -> String {
    describe(unix, &local_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-03-15 (a Friday) 10:30:00 UTC.
    const T: u64 = 1_710_498_600;

    #[test]
    fn the_local_date_weekday_tomorrow_and_the_days_after_are_spelled_out() {
        let s = describe(T, &|_| (3600, "CET".into()));
        assert!(s.starts_with("Now it is Friday 2024-03-15 11:30 CET (UTC+01:00)."), "{s}");
        assert!(s.contains("UTC: Friday 2024-03-15 10:30"), "{s}");
        assert!(s.contains(&format!("Unix: {T}.")), "{s}");
        assert!(s.contains("Tomorrow is Saturday 2024-03-16."), "{s}");
        assert!(s.contains("After that: Sunday 2024-03-17, Monday 2024-03-18"), "{s}");
        assert!(s.ends_with("Friday 2024-03-22."), "a week on: {s}");
    }

    #[test]
    fn local_midnight_can_be_a_different_day_from_utc() {
        // 23:40 on Thursday in UTC is 00:40 on Friday in UTC+1: "tomorrow" is the local one.
        let s = describe(T - 10 * 3600 - 50 * 60, &|_| (3600, "CET".into()));
        assert!(s.starts_with("Now it is Friday 2024-03-15 00:40 CET"), "{s}");
        assert!(s.contains("UTC: Thursday 2024-03-14 23:40"), "{s}");
        assert!(s.contains("Tomorrow is Saturday 2024-03-16."), "{s}");
    }

    #[test]
    fn west_of_greenwich_and_a_leap_day() {
        // 2024-02-29 02:00 UTC is still the 28th in UTC-5.
        let s = describe(1_709_172_000, &|_| (-5 * 3600, "EST".into()));
        assert!(s.starts_with("Now it is Wednesday 2024-02-28 21:00 EST (UTC-05:00)."), "{s}");
        assert!(s.contains("Tomorrow is Thursday 2024-02-29."), "{s}");
        assert!(s.contains("Friday 2024-03-01"), "{s}");
    }

    #[test]
    fn the_systems_offset_text_is_read_and_this_machines_own_is_sane() {
        assert_eq!(parse_offset("+0200 CEST"), Some((7200, "CEST".into())));
        assert_eq!(parse_offset("-0330 NST"), Some((-12_600, "NST".into())));
        assert_eq!(parse_offset("+0000 UTC"), Some((0, "UTC".into())));
        assert_eq!(parse_offset("garbage"), None);
        assert_eq!(parse_offset("0200 CEST"), None);
        let (off, zone) = local_offset(T);
        assert!((-14 * 3600..=14 * 3600).contains(&off), "{off}");
        assert!(!zone.is_empty());
    }
}
