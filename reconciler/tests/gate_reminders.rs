//! The `events` reminders gate, ported from `components/events-domain/e2e-reminders.sh`.
//!
//! THIS ONE GAINS COVERAGE RATHER THAN SPEED: the shell gate needs MailHog, which CI cannot install, so
//! the notification fan-out had no coverage on a pull request. `gatelib::MailSink` receives the SMTP
//! directly; the chain is otherwise unchanged (the component posts to `mail:gateway-url`,
//! `comp-mailrelay` turns that into SMTP).

mod gatelib;
use gatelib::{field, Gate, MailRelay, MailSink};
use serde_json::{json, Value};

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

/// `starts_at` two hours out, RFC 3339 UTC.
fn in_two_hours() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is before 1970")
        .as_secs()
        + 2 * 3600;
    gatelib::rfc3339(secs)
}

#[test]
fn a_reminder_is_scheduled_fired_once_and_reaches_a_real_mailbox() {
    let mail = MailSink::start();
    let Some(relay) = MailRelay::start(&mail.addr()) else { return };

    let gateway = format!("mail:gateway-url={}", relay.url());
    let config = [
        "allow-test-routes=1",
        "allowed-types=image/png,image/jpeg,image/webp",
        "max-size=2097152",
        gateway.as_str(),
        "mail:from=events@holon.test",
    ];
    let relay_egress = relay.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("events", "events-domain", &config, &[&relay_egress])
    else {
        return;
    };

    let seed = gate.seed();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee) = (tok("organizer"), tok("attendee"));

    // A per-run marker, as the shell used `$$`: the mailbox is asserted by content.
    let run = format!("r{}", std::process::id());

    let (_, far) = gate.post(
        "/api/events",
        Some(&organizer),
        json!({"title":"Far Away","starts_at":"2027-01-01T18:00:00Z","capacity":10}),
    );
    let fid = field(&far, "id");
    let (_, peek) = gate.get(&format!("/api/events/{fid}/reminder"), Some(&organizer));
    assert!(
        peek.contains("\"scheduled\":true"),
        "creating an event must put its reminder on the clock: {peek}"
    );
    let due_in = parse(&peek)["due_in_seconds"].as_i64().unwrap_or(0);
    assert!(
        due_in > 0,
        "an event in 2027 must have a reminder in the FUTURE, not {due_in} seconds ago"
    );

    // Nothing fires for it.
    let (_, r) = gate.post("/api/reminders/run", Some(&organizer), json!({}));
    assert_eq!(field(&r, "fired"), "0", "a reminder that is not due yet must not fire");

    let soon = in_two_hours();
    let (_, ev) = gate.post(
        "/api/events",
        Some(&organizer),
        json!({"title": format!("Tonight {run}"), "starts_at": soon, "capacity": 10}),
    );
    let eid = field(&ev, "id");
    assert!(!eid.is_empty(), "could not create the soon event: {ev}");

    let (_, t) = gate.post(&format!("/api/events/{eid}/tickets"), Some(&attendee), json!({}));
    assert!(!field(&t, "id").is_empty(), "no ticket: {t}");

    let (_, put) = gate.json(
        "PUT",
        "/api/prefs",
        Some(&attendee),
        Some(json!({
        "default_channels": ["in-app", "email"],
        "email_address": format!("ada-{run}@example.test"),
        "overrides": {}})),
    );
    assert!(put.contains("\"ok\":true"), "could not set preferences: {put}");

    let before_mail = mail.count_containing(&format!("Tonight {run}"));

    let (_, run_out) = gate.post("/api/reminders/run", Some(&organizer), json!({}));
    let fired = field(&run_out, "fired");
    assert_ne!(fired, "0", "a reminder that is due must fire: {run_out}");

    let (_, notes) = gate.get("/api/notifications", Some(&attendee));
    assert!(
        notes.contains("\"kind\":\"event-reminder\""),
        "no event-reminder in the inbox: {notes}"
    );
    assert!(
        notes.contains(&format!("Tonight {run}")),
        "the reminder does not name the event: {notes}"
    );

    std::thread::sleep(std::time::Duration::from_millis(500));
    let after_mail = mail.count_containing(&format!("Tonight {run}"));
    assert!(
        after_mail > before_mail,
        "the mailbox holds no reminder for this event — the fan-out reported success and nothing arrived"
    );

    // --- firing twice does not remind twice ---
    // `ack` is what makes that true: a reminder repeating on every scheduler tick is worse than none.
    let (_, again) = gate.post("/api/reminders/run", Some(&organizer), json!({}));
    assert_eq!(
        field(&again, "fired"),
        "0",
        "an acked reminder fired again — it would repeat on every tick"
    );

    // --- moving the event moves its reminder ---
    // A PATCH changing `starts_at` must re-schedule it, or it goes out a day before a start that is no more.
    let (_, mv) = gate.post(
        "/api/events",
        Some(&organizer),
        json!({"title": format!("Moved {run}"), "starts_at": "2027-01-01T18:00:00Z", "capacity": 5}),
    );
    let mid = field(&mv, "id");
    let first = parse(&mv)["reminder_at"].as_u64().unwrap_or(0);
    let (c, patched) = gate.patch(
        &format!("/api/events/{mid}"),
        Some(&organizer),
        json!({"starts_at": "2027-02-01T18:00:00Z"}),
    );
    assert_eq!(c, 200, "moving an event answered {c}: {patched}");
    let (_, rem) = gate.get(&format!("/api/events/{mid}/reminder"), Some(&organizer));
    let run_at = parse(&rem)["run_at"].as_u64().unwrap_or(0);
    // 2027-02-01T18:00:00Z less 24 hours.
    assert_eq!(
        run_at, 1_801_418_400,
        "the event moved to 2027-02-01T18:00:00Z and its reminder is still set for {run_at} \
         (it was {first} before the move) — it must go out 24 hours before the NEW start: {rem}"
    );
    assert_eq!(
        parse(&patched)["reminder_at"].as_u64(),
        Some(run_at),
        "the PATCH answer must say when the moved reminder goes out: {patched}"
    );
    // Amending anything else leaves the reminder alone.
    let (_, _) = gate.patch(
        &format!("/api/events/{mid}"),
        Some(&organizer),
        json!({"title": "Moved, renamed"}),
    );
    let (_, rem2) = gate.get(&format!("/api/events/{mid}/reminder"), Some(&organizer));
    assert_eq!(
        parse(&rem2)["run_at"].as_u64(),
        Some(run_at),
        "a PATCH that does not touch `starts_at` must leave the reminder where it is: {rem2}"
    );

    let (_, c) = gate.post(
        "/api/events",
        Some(&organizer),
        json!({"title": format!("Doomed {run}"), "starts_at": soon, "capacity": 5}),
    );
    let cid = field(&c, "id");
    gate.post(&format!("/api/events/{cid}/tickets"), Some(&attendee), json!({}));
    gate.delete(&format!("/api/events/{cid}"), Some(&organizer));
    let (_, rem) = gate.get(&format!("/api/events/{cid}/reminder"), Some(&organizer));
    assert!(
        rem.contains("\"scheduled\":false"),
        "cancelling an event must take its reminder off the clock — it must not still tell people to come: {rem}"
    );
    let (_, notes) = gate.get("/api/notifications", Some(&attendee));
    assert!(
        notes.contains("\"kind\":\"event-cancelled\""),
        "cancelling an event must tell the people holding tickets for it: {notes}"
    );
}
