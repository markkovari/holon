//! `notify` — one call, two channels, and a real email in a real mailbox.
//!
//! Ported from `components/notify-probe/e2e.sh`, the tenth and last composition gate
//! to move. The assertion that matters is the last kind: the mailbox is ASKED what it
//! actually holds, and the negative case is asserted too — a subject who opted out of
//! email must produce no email at all. A notification system that cannot be turned
//! off is a mailing list.
//!
//! ## This one gains CI coverage the shell version can never have
//!
//! `e2e.sh` calls `notify_start_mail`, which wants MailHog on `$PATH`. MailHog is
//! archived, predates Go modules, and does not `go install` on the CI runner — so the
//! shell gate is skipped there and the whole notification fan-out was a local-only
//! check. `gatelib::MailSink` is a pure-Rust SMTP server on a port the OS picks, so
//! this port needs nothing installed and runs in CI like any other gate.
//!
//! `comp-mailrelay` is still the thing that speaks SMTP to it, and is the only piece
//! of this picture that is not the component under test.

mod gatelib;
use gatelib::{field, Gate, MailRelay, MailSink};
use serde_json::{json, Value};

const CRATE: &str = "notify-probe";

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

/// The channels a `/notify` response says it ATTEMPTED, in order.
fn channels(out: &str) -> Vec<String> {
    parse(out)["outcomes"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["channel"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Whether `channel` was attempted AND reported ok.
fn ok_on(out: &str, channel: &str) -> bool {
    parse(out)["outcomes"]
        .as_array()
        .map(|a| {
            a.iter().any(|o| {
                o["channel"].as_str() == Some(channel)
                    && (o["ok"] == json!(true) || o["status"].as_str() == Some("ok"))
            })
        })
        .unwrap_or(false)
}

#[test]
fn one_call_reaches_an_inbox_and_a_real_mailbox_and_opting_out_is_real() {
    let mail = MailSink::start();
    let Some(relay) = MailRelay::start(&mail.addr()) else { return };

    let gateway = format!("mail:gateway-url={}", relay.url());
    let config = [gateway.as_str(), "mail:from=notify@holon.test"];
    let relay_egress = relay.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("notify", CRATE, &config, &[&relay_egress])
    else {
        return;
    };

    // Both are capabilities this component CALLS. A fan-out that stored its own
    // messages would be a second inbox nothing else can read, and email that did not
    // leave through `mail:send` would be this component deciding a deploy-time choice.
    gatelib::requires_capability(
        "notify-prefs",
        "notify:inbox/inbox",
        "the in-app channel is a capability this component CALLS — a fan-out that stored \
         its own messages would be a second inbox nothing else can read",
    );
    gatelib::requires_capability(
        "notify-prefs",
        "mail:send/sender",
        "email leaves through mail:send, whose backend is a deploy-time choice — Resend or \
         the local relay — and not something this component decides",
    );

    // Unique per run, because the mailbox and the inbox both outlive one assertion.
    let run = format!("run{}", std::process::id());
    let (ada, bob) = (format!("ada-{run}"), format!("bob-{run}"));

    // --- a subject nobody has configured -----------------------------------
    //
    // Not an error, and in-app only: the setting that cannot deliver anything anywhere
    // it should not is the right default for one nobody chose.
    let (_, def) = gate.get(&format!("/prefs?subject={ada}"), None);
    assert!(def.contains("\"in-app\""), "an unconfigured subject must default to in-app: {def}");
    assert!(!def.contains("\"email\""), "an unconfigured subject must NOT default to email: {def}");

    // --- opt in to both ----------------------------------------------------
    let put = |subject: &str, chans: Value, overrides: Value| {
        gate.send(
            "PUT",
            "/prefs",
            None,
            Some((
                "application/json",
                json!({
                    "subject": subject,
                    "default_channels": chans,
                    "email_address": format!("{subject}@example.test"),
                    "overrides": overrides,
                })
                .to_string()
                .into_bytes(),
            )),
        )
    };
    let (_, p) = put(&ada, json!(["in-app", "email"]), json!({}));
    assert!(p.contains("\"ok\":true"), "could not set preferences: {p}");

    let marker = format!("marker-{run}-both");
    let (_, out) = gate.post(
        "/notify",
        None,
        json!({
            "subject": ada, "kind": "ticket-swapped",
            "title": "Your ticket was swapped", "body": marker, "payload": "tkt_1",
        }),
    );
    assert!(ok_on(&out, "in-app"), "the in-app channel did not deliver: {out}");
    assert!(ok_on(&out, "email"), "the email channel did not deliver: {out}");

    // --- it is IN the inbox ------------------------------------------------
    let (_, box_) = gate.get(&format!("/inbox?subject={ada}&after=0&limit=10"), None);
    assert!(box_.contains(&marker), "the note is not in the inbox: {box_}");
    assert!(box_.contains("\"kind\":\"ticket-swapped\""), "the note lost its kind: {box_}");
    let (_, unread) = gate.get(&format!("/unread?subject={ada}"), None);
    assert_eq!(
        field(&unread, "unread"),
        "1",
        "one delivered note is one unread, not '{}'",
        field(&unread, "unread")
    );

    // --- and a REAL email is in a REAL mailbox -----------------------------
    //
    // Delivered over genuine SMTP by comp-mailrelay. Polled rather than slept: the
    // shell version sleeps 0.5s, which is either too long or not long enough.
    let mut found = 0usize;
    for _ in 0..40 {
        found = mail.count_containing(&marker);
        if found > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        found > 0,
        "the mailbox holds no message containing {marker} — the send reported success and \
         nothing arrived"
    );
    let held = mail.messages_containing(&marker);
    let msg = held.first().expect("a message that contains the marker");
    assert!(
        msg.contains(&format!("{ada}@example.test")),
        "the email did not go to the address in the preference: {msg:.400}"
    );
    assert!(
        msg.contains("Your ticket was swapped"),
        "the subject line did not survive: {msg:.400}"
    );

    // --- reading it --------------------------------------------------------
    let seq = parse(&box_)["notes"][0]["seq"].as_i64().unwrap_or(-1);
    assert!(seq >= 0, "the inbox note has no seq to read: {box_}");
    let (_, marked) = gate.post("/read", None, json!({"subject": ada, "seqs": [seq]}));
    assert_eq!(
        field(&marked, "marked"),
        "1",
        "marking one note read reported '{}'",
        field(&marked, "marked")
    );
    let (_, u) = gate.get(&format!("/unread?subject={ada}"), None);
    assert_eq!(field(&u, "unread"), "0", "after reading the only note the badge must be 0");

    // Twice must not drive it negative — a client that retries is not a client that
    // should make the badge wrong.
    gate.post("/read", None, json!({"subject": ada, "seqs": [seq]}));
    let (_, u) = gate.get(&format!("/unread?subject={ada}"), None);
    assert_eq!(field(&u, "unread"), "0", "marking the same note read twice moved the badge");

    // --- the cursor is a cursor --------------------------------------------
    gate.post(
        "/notify",
        None,
        json!({
            "subject": ada, "kind": "event-cancelled", "title": "Cancelled",
            "body": format!("second-{run}"), "payload": "",
        }),
    );
    let (_, tail) = gate.get(&format!("/inbox?subject={ada}&after={seq}&limit=10"), None);
    assert!(
        tail.contains(&format!("second-{run}")),
        "after={seq} must return what came next: {tail}"
    );
    assert!(!tail.contains(&marker), "after={seq} must NOT return what came before it: {tail}");

    // --- opting out of email is REAL ---------------------------------------
    let quiet = format!("quiet-{run}");
    let before = mail.count_containing(&quiet);
    put(&bob, json!(["in-app"]), json!({}));
    let (_, out) = gate.post(
        "/notify",
        None,
        json!({
            "subject": bob, "kind": "ticket-swapped", "title": "Quiet",
            "body": quiet, "payload": "",
        }),
    );
    assert!(
        !channels(&out).iter().any(|c| c == "email"),
        "a subject who did not ask for email must get no email OUTCOME at all: {out}"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = mail.count_containing(&quiet);
    assert_eq!(
        after, before,
        "an opted-out subject received {after} email(s) — opting out has to be real, not \
         cosmetic"
    );
    let (_, bobbox) = gate.get(&format!("/inbox?subject={bob}&after=0&limit=5"), None);
    assert!(bobbox.contains(&quiet), "opting out of email must not cost the in-app copy: {bobbox}");

    // --- muting ONE kind ---------------------------------------------------
    //
    // An empty override is a real answer and means "not this one". Falling back to the
    // defaults on an empty list would make muting a single kind impossible.
    put(&ada, json!(["in-app", "email"]), json!({"noisy": []}));
    let muted = format!("muted-{run}");
    let (_, out) = gate.post(
        "/notify",
        None,
        json!({
            "subject": ada, "kind": "noisy", "title": "Muted",
            "body": muted, "payload": "",
        }),
    );
    assert!(
        channels(&out).is_empty(),
        "a kind muted with an empty override must attempt NOTHING, but tried: {:?}",
        channels(&out)
    );
    let (_, all) = gate.get(&format!("/inbox?subject={ada}&after=0&limit=20"), None);
    assert!(!all.contains(&muted), "a muted kind reached the inbox anyway: {all}");

    // ...and the same subject still gets the kinds they did not mute.
    let (_, out) = gate.post(
        "/notify",
        None,
        json!({
            "subject": ada, "kind": "ticket-swapped", "title": "Still on",
            "body": format!("still-{run}"), "payload": "",
        }),
    );
    assert!(ok_on(&out, "email"), "muting one kind muted the others: {out}");
}
