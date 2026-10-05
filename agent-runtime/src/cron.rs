//! Schedule expressions: 5-field cron (UTC), `@hourly|@daily|@weekly|@monthly`,
//! and `@every <N>s|m|h`. Day-of-month vs day-of-week follows Vixie cron: if
//! both are restricted a day matches when EITHER does.

#[derive(Debug, PartialEq)]
pub enum Schedule {
    Every(u64),
    Cron(Box<Fields>),
}

#[derive(Debug, PartialEq)]
pub struct Fields {
    min: Vec<bool>,  // 60
    hour: Vec<bool>, // 24
    dom: Vec<bool>,  // 32 (1..=31)
    mon: Vec<bool>,  // 13 (1..=12)
    dow: Vec<bool>,  // 7  (0 = Sunday)
    dom_star: bool,
    dow_star: bool,
}

pub fn parse(expr: &str) -> Result<Schedule, String> {
    let e = expr.trim();
    let expanded = match e {
        "@hourly" => "0 * * * *",
        "@daily" | "@midnight" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        _ => e,
    };
    if let Some(rest) = expanded.strip_prefix("@every") {
        let rest = rest.trim();
        let (num, unit) =
            rest.split_at(rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len()));
        let n: u64 =
            num.parse().map_err(|_| format!("`{expr}`: @every needs a number, e.g. @every 5m"))?;
        let mult = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            _ => return Err(format!("`{expr}`: unit must be s, m or h")),
        };
        if n == 0 {
            return Err(format!("`{expr}`: interval must be > 0"));
        }
        return Ok(Schedule::Every(n * mult));
    }
    let f: Vec<&str> = expanded.split_whitespace().collect();
    if f.len() != 5 {
        return Err(format!("`{expr}`: expected 5 fields (min hour dom month dow)"));
    }
    let dom_star = f[2] == "*";
    let dow_star = f[4] == "*";
    Ok(Schedule::Cron(Box::new(Fields {
        min: field(f[0], 0, 59, &[])?,
        hour: field(f[1], 0, 23, &[])?,
        dom: field(f[2], 1, 31, &[])?,
        mon: field(
            f[3],
            1,
            12,
            &["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"],
        )?,
        dow: dow_field(f[4])?,
        dom_star,
        dow_star,
    })))
}

fn dow_field(s: &str) -> Result<Vec<bool>, String> {
    // 7 is Sunday too; fold it onto 0.
    let mut v = field(s, 0, 7, &["sun", "mon", "tue", "wed", "thu", "fri", "sat"])?;
    if v[7] {
        v[0] = true;
    }
    v.truncate(7);
    Ok(v)
}

/// `names[i]` is the name of value `lo + i`.
fn field(s: &str, lo: usize, hi: usize, names: &[&str]) -> Result<Vec<bool>, String> {
    let mut set = vec![false; hi + 1];
    let val = |t: &str| -> Result<usize, String> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(t)) {
            return Ok(lo + i);
        }
        let n: usize = t.parse().map_err(|_| format!("bad value `{t}`"))?;
        if n < lo || n > hi {
            return Err(format!("{n} out of range {lo}-{hi}"));
        }
        Ok(n)
    };
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (r, st.parse::<usize>().map_err(|_| format!("bad step `{st}`"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err("step must be > 0".into());
        }
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (val(a)?, val(b)?)
        } else {
            let v = val(range)?;
            // `5/10` means 5, 15, 25...
            if part.contains('/') {
                (v, hi)
            } else {
                (v, v)
            }
        };
        if a > b {
            return Err(format!("range {a}-{b} runs backwards"));
        }
        let mut i = a;
        while i <= b {
            set[i] = true;
            i += step;
        }
    }
    Ok(set)
}

/// (minute, hour, day-of-month, month, day-of-week 0=Sun) in UTC.
pub fn civil(unix: u64) -> (usize, usize, usize, usize, usize) {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as usize;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let dow = ((days + 4).rem_euclid(7)) as usize; // 1970-01-01 was a Thursday
    ((secs % 3600 / 60) as usize, (secs / 3600) as usize, d, m, dow)
}

impl Fields {
    pub fn matches(&self, unix: u64) -> bool {
        let (mi, h, d, mo, w) = civil(unix);
        let day = match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (false, true) => self.dom[d],
            (true, false) => self.dow[w],
            (false, false) => self.dom[d] || self.dow[w],
        };
        self.min[mi] && self.hour[h] && self.mon[mo] && day
    }
}

impl Schedule {
    /// Should this schedule fire at `now`, given when it last fired (`None` =
    /// never, in which case an `@every` waits one full interval from
    /// `started` rather than firing the instant the runtime boots)?
    pub fn due(&self, now: u64, last: Option<u64>, started: u64) -> bool {
        match self {
            Schedule::Every(n) => now >= last.unwrap_or(started) + n,
            Schedule::Cron(f) => f.matches(now) && last.is_none_or(|l| l / 60 != now / 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-03-15 (a Friday) 10:30:00 UTC.
    const T: u64 = 1_710_498_600;

    #[test]
    fn civil_time() {
        assert_eq!(civil(T), (30, 10, 15, 3, 5));
        assert_eq!(civil(0), (0, 0, 1, 1, 4));
    }

    #[test]
    fn cron_matching() {
        let m = |e: &str| match parse(e).unwrap() {
            Schedule::Cron(f) => f.matches(T),
            _ => panic!(),
        };
        assert!(m("30 10 * * *"));
        assert!(m("*/15 * * * *"));
        assert!(m("30 10 15 3 5"));
        assert!(m("30 10 * mar fri"));
        assert!(!m("31 10 * * *"));
        assert!(m("30 10 1 * 5"), "dom OR dow when both are restricted");
        assert!(!m("30 10 1 * 1"));
        assert!(m("0,30 9-11 * * *"));
        assert!(m("30 10 * * 5-7"));
    }

    #[test]
    fn macros_and_every() {
        assert_eq!(parse("@every 5m").unwrap(), Schedule::Every(300));
        assert!(matches!(parse("@hourly").unwrap(), Schedule::Cron(_)));
        assert!(parse("@every 0s").is_err());
        assert!(parse("@every 5d").is_err());
        assert!(parse("* * * *").is_err());
        assert!(parse("61 * * * *").is_err());
    }

    #[test]
    fn due_logic() {
        let every = parse("@every 60s").unwrap();
        assert!(!every.due(100, None, 100), "does not fire the instant it starts");
        assert!(every.due(160, None, 100));
        assert!(!every.due(200, Some(160), 100));
        let c = parse("30 10 * * *").unwrap();
        assert!(c.due(T, None, 0));
        assert!(!c.due(T + 20, Some(T), 0), "once per matching minute");
    }
}
