//! Reminders, and telling people things.
//!
//! ## Why a timer and not a check on read
//!
//! "Notify everyone 24 hours before it starts" is a time in the future, and a
//! request handler cannot wait for one. The alternative — work out on every read
//! whether a reminder is due — sends nothing at all if nobody happens to load the
//! page, and sends it twice if two people load it at once. `sched:timer` holds the
//! job, leases it to one caller when it is due, and takes an `ack`.
//!
//! ## What the app does not decide
//!
//! Whether a reminder becomes an email, an in-app note, both or neither is
//! `notify:prefs`' answer, read from what the person set. This module calls
//! `notify` and reads back what happened; it never picks a channel. An app that
//! sent its own email would be re-deciding, for every one of its users, a question
//! they had already answered.

use serde_json::json;

use crate::bindings::notify::prefs::preferences as notify;
use crate::bindings::sched::timer::timer;
use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::wasi::http::types::Method;
use crate::store::{find_by_str, load};
use crate::{require, Reply, Route};

/// How long before an event its reminder goes out.
pub const LEAD_SECONDS: u64 = 24 * 60 * 60;

/// One job per event, and the key says which — so scheduling twice replaces rather
/// than duplicates, and cancelling an event can find the job without an index.
fn job_key(event_id: &str) -> String {
    format!("reminder:{event_id}")
}

pub fn now() -> u64 {
    wall_clock::now().seconds
}

/// `2026-09-01T18:00:00Z` -> unix seconds.
///
/// By hand, because the alternative is a date crate in a component whose entire
/// need is one shape of one format — the one this app's own contract specifies.
/// Anything that is not that shape returns `None` and the caller declines to
/// schedule, rather than scheduling for the epoch and firing immediately.
pub fn parse_iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from the civil epoch — Howard Hinnant's algorithm, which is exact for
    // every proleptic Gregorian date and shorter than the table of month lengths
    // and leap rules it replaces.
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec;
    u64::try_from(secs).ok()
}

/// Put an event's reminder on the clock. Absent or unparseable `starts_at`, or an
/// event less than the lead time away, schedules nothing — and says so rather than
/// firing a reminder for something that has already happened.
pub fn schedule(event_id: &str, starts_at: &str) -> Option<u64> {
    let start = parse_iso(starts_at)?;
    let at = start.checked_sub(LEAD_SECONDS)?;
    let payload = json!({ "event_id": event_id }).to_string();
    timer::schedule_at(&job_key(event_id), at, payload.as_bytes()).ok()?;
    Some(at)
}

pub fn cancel(event_id: &str) {
    // A cancelled event must not still remind people to come to it. Failure is
    // ignored on purpose: there may be no job, which is not a problem to report.
    let _ = timer::cancel(&job_key(event_id));
}

/// Everyone holding a live ticket for this event.
pub fn holders_of(event_id: &str) -> Vec<String> {
    find_by_str("tickets", "event_id", event_id)
        .into_iter()
        .filter_map(|e| {
            let d: serde_json::Value = serde_json::from_str(&e.data).ok()?;
            // A released ticket is not a reason to come; a checked-in one means they
            // are already here.
            if d["state"].as_str() != Some("issued") {
                return None;
            }
            d["holder"].as_str().map(str::to_string)
        })
        .collect()
}

/// Tell one subject, through their own preferences, and report what happened.
pub fn tell(subject: &str, kind: &str, title: &str, body: &str, payload: &str) -> serde_json::Value {
    match notify::notify(subject, kind, title, body, payload) {
        Ok(outcomes) => json!(outcomes
            .iter()
            .map(|o| json!({
                "channel": match o.channel {
                    notify::Channel::InApp => "in-app",
                    notify::Channel::Email => "email",
                },
                "ok": o.ok,
                "detail": o.detail,
            }))
            .collect::<Vec<_>>()),
        Err(e) => json!([{ "channel": "none", "ok": false, "detail": format!("{e:?}") }]),
    }
}

/// `POST /api/reminders/run` — fire whatever is due.
///
/// A route rather than a background loop because a component has no loop: it runs
/// when something calls it. In a deployment that caller is `comp-relay` on a
/// schedule (the `[triggers]` block in an app spec); in a demo it is a button. The
/// work is identical either way, which is the point of it being a route.
pub fn run(method: &Method, route: &Route) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(404, "not_found");
    }
    // Only an organizer or admin may make the clock tick by hand.
    if let Err(r) = require(route, "event", "write") {
        return r;
    }

    // A 60-second lease: long enough that this run finishes, short enough that a
    // crash re-offers the job rather than losing it.
    let due = match timer::due(now(), 20, 60) {
        Ok(jobs) => jobs,
        Err(e) => return Reply::err(500, &format!("timer: {e:?}")),
    };

    let mut fired = Vec::new();
    for job in &due {
        let payload: serde_json::Value =
            serde_json::from_slice(&job.payload).unwrap_or_else(|_| json!({}));
        let event_id = payload["event_id"].as_str().unwrap_or_default().to_string();
        let Ok((_, event)) = load("events", &event_id) else {
            // The event is gone; the job should be too.
            let _ = timer::ack(&job.key);
            continue;
        };
        if event["state"].as_str() != Some("open") {
            let _ = timer::ack(&job.key);
            continue;
        }
        let title = event["title"].as_str().unwrap_or("your event").to_string();
        let starts = event["starts_at"].as_str().unwrap_or_default().to_string();
        let mut told = Vec::new();
        for subject in holders_of(&event_id) {
            told.push(json!({
                "subject": subject,
                "outcomes": tell(
                    &subject,
                    "event-reminder",
                    &format!("Tomorrow: {title}"),
                    &format!("{title} starts at {starts}. Your ticket is in your wallet."),
                    &json!({ "event_id": event_id }).to_string(),
                ),
            }));
        }
        // Ack AFTER telling everyone: a crash in the middle re-offers the job when
        // the lease expires, and a second reminder is a smaller wrong than none.
        let _ = timer::ack(&job.key);
        fired.push(json!({ "event_id": event_id, "title": title, "told": told }));
    }

    Reply::json(200, json!({ "fired": fired.len(), "reminders": fired }))
}

/// `GET /api/events/{id}/reminder` — when it will go out, if it will.
pub fn peek(route: &Route, event_id: &str) -> Reply {
    if let Err(r) = require(route, "event", "read") {
        return r;
    }
    if let Err(r) = load("events", event_id) {
        return r;
    }
    match timer::peek(&job_key(event_id)) {
        Ok(Some(job)) => Reply::json(
            200,
            json!({ "scheduled": true, "run_at": job.run_at, "now": now(),
                    "due_in_seconds": job.run_at.saturating_sub(now()) }),
        ),
        Ok(None) => Reply::json(200, json!({ "scheduled": false, "now": now() })),
        Err(e) => Reply::err(500, &format!("timer: {e:?}")),
    }
}
