//! `ical` — write an .ics calendar file — RFC 5545, with the line folding, escaping and UTC timestamps it requires
//!
//! A correct, dependency-free RFC 5545 writer: events in, a `.ics` VCALENDAR
//! document out. Handles the parts that actually bite — CRLF line endings,
//! 75-octet line folding (continuation lines begin with a space), escaping of
//! `\ ; , ` and newlines in text values, and UTC timestamps in the compact
//! `YYYYMMDDTHHMMSSZ` basic format. Optional per-event RRULE and a VALARM.
//!
//! Pure compute — no state, no host imports. Timestamps are converted to civil
//! dates with Howard Hinnant's `civil_from_days` (proleptic Gregorian), so no
//! date library is needed.
//!
//! ponytail: publish-only. Parsing arbitrary third-party .ics is a much larger
//! job and out of scope — this covers "download .ics" and "subscribe to a feed".

#[allow(warnings)]
mod bindings;

use bindings::exports::ical::codec::codec::{Event, Guest};

struct Component;

/// Civil (year, month, day) from days since the Unix epoch — Hinnant's
/// algorithm, valid across the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

/// Unix seconds -> `YYYYMMDDTHHMMSSZ` (UTC basic format).
fn ts(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Escape a text value per RFC 5545 §3.3.11.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Fold a content line to <= 75 octets, continuation lines starting with a
/// single space (RFC 5545 §3.1). Operates on bytes but never splits a UTF-8
/// sequence. Returns the line WITHOUT a trailing CRLF.
fn fold(line: &str) -> String {
    let bytes = line.as_bytes();
    if bytes.len() <= 75 {
        return line.to_string();
    }
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 74 * 3);
    let mut i = 0;
    let mut budget = 75; // first line: 75 octets; continuations: 74 (leading space)
    while i < bytes.len() {
        // take up to `budget` bytes without splitting a UTF-8 char boundary
        let mut end = (i + budget).min(bytes.len());
        while end > i && (bytes[end - 1] & 0xC0) == 0x80 {
            end -= 1; // back off into the middle of a multibyte char
        }
        if end == i {
            end = (i + budget).min(bytes.len()); // pathological; take raw
        }
        if i > 0 {
            out.push_str("\r\n ");
        }
        out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
        i = end;
        budget = 74;
    }
    out
}

/// Emit one VEVENT block (lines pushed, unfolded — folding happens at assembly).
fn vevent(ev: &Event, lines: &mut Vec<String>) {
    lines.push("BEGIN:VEVENT".into());
    lines.push(format!("UID:{}", esc(&ev.uid)));
    lines.push(format!("DTSTAMP:{}", ts(ev.start)));
    lines.push(format!("DTSTART:{}", ts(ev.start)));
    lines.push(format!("DTEND:{}", ts(if ev.end > ev.start { ev.end } else { ev.start })));
    lines.push(format!("SUMMARY:{}", esc(&ev.summary)));
    if !ev.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", esc(&ev.description)));
    }
    if !ev.location.is_empty() {
        lines.push(format!("LOCATION:{}", esc(&ev.location)));
    }
    if !ev.organizer.is_empty() {
        lines.push(format!("ORGANIZER:mailto:{}", ev.organizer));
    }
    if !ev.rrule.is_empty() {
        lines.push(format!("RRULE:{}", ev.rrule));
    }
    if ev.alarm_minutes > 0 {
        lines.push("BEGIN:VALARM".into());
        lines.push("ACTION:DISPLAY".into());
        lines.push(format!("DESCRIPTION:{}", esc(&ev.summary)));
        lines.push(format!("TRIGGER:-PT{}M", ev.alarm_minutes));
        lines.push("END:VALARM".into());
    }
    lines.push("END:VEVENT".into());
}

/// Wrap events in a VCALENDAR, fold every line, join with CRLF.
fn calendar(events: &[Event], prod_id: &str, cal_name: &str) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        format!("PRODID:-//{}//EN", if prod_id.is_empty() { "comp//ical" } else { prod_id }),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
    ];
    if !cal_name.is_empty() {
        lines.push(format!("X-WR-CALNAME:{}", esc(cal_name)));
    }
    for ev in events {
        vevent(ev, &mut lines);
    }
    lines.push("END:VCALENDAR".to_string());
    // every line folded, CRLF-terminated (including the last — RFC 5545 wants it).
    lines.iter().map(|l| format!("{}\r\n", fold(l))).collect()
}

impl Guest for Component {
    fn format_event(ev: Event, prod_id: String) -> String {
        calendar(std::slice::from_ref(&ev), &prod_id, "")
    }
    fn format_calendar(events: Vec<Event>, prod_id: String, cal_name: String) -> String {
        calendar(&events, &prod_id, &cal_name)
    }
}

bindings::export!(Component with_types_in bindings);
