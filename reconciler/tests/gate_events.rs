//! The `events` gate, as a Rust integration test rather than a bash script.
//!
//! A PROTOTYPE asserting exactly what `components/events-domain/e2e-events.sh` does, minus the
//! installed tools. The gates are distributed across machines, and every external tool a gate needs
//! (`curl`, `python3`, `grep`, `cargo`, `MailHog`, `docker`, ...) is a way one worker can differ from
//! another. This needs `comp-host` and a composed `.wasm`, artifacts this repository builds and can ship
//! to a worker.
//!
//! Every failure message is verbatim: ADR-0088 says a gate's output IS the next prompt. One host process
//! serves the whole gate, where the bash version spawns 26 `curl` and 5 `python3`.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const APP: &str = "events";
const COMPOSED: &str = "events_domain.composed.wasm";
const CONFIG: &[&str] =
    &["allow-test-routes=1", "allowed-types=image/png,image/jpeg,image/webp", "max-size=2097152"];

/// A real PNG, byte for byte, as the bash gate writes with `printf`. The round trip is the point: the
/// router once read every body through `from_utf8_lossy`, so an upload stored something that is not an image.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', b'x', 0x9c, b'c', 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
    b'B', b'`', 0x82,
];

#[test]
fn events_created_read_amended_and_cancelled() {
    let Some(gate) = Gate::start(APP, COMPOSED, CONFIG) else { return };

    let seed = gate.seed();
    let raw = seed.to_string();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee) = (tok("organizer"), tok("attendee"));
    assert!(
        !seed["event_id"].as_str().unwrap_or_default().is_empty()
            && !organizer.is_empty()
            && !attendee.is_empty()
            && !tok("other").is_empty(),
        "the fixture did not come back with an event and three tokens: {raw}"
    );

    let (code, _) = gate.json(
        "POST",
        "/api/events",
        None,
        Some(json!({"title":"x","starts_at":"2026-09-01T18:00:00Z","capacity":5})),
    );
    assert!(
        code == 401 || code == 403,
        "an unauthenticated POST /api/events must be refused (got {code})"
    );

    let (_, new) = gate.json(
        "POST",
        "/api/events",
        Some(&organizer),
        Some(json!({"title":"Wasm Night","starts_at":"2026-10-01T18:00:00Z","capacity":50})),
    );
    let new_id = field(&new, "id");
    assert!(!new_id.is_empty(), "POST /api/events returned no id: {new}");

    let doc = gate.stored("events", &new_id);
    for want in ["\"title\"", "\"starts_at\"", "\"capacity\"", "\"organizer\"", "\"state\""] {
        assert!(
            doc.contains(want),
            "the stored event is missing {want} — CONTRACT.md fixes the shape: {doc}"
        );
    }
    assert!(
        doc.contains("\"state\":\"open\"") || doc.contains("\"state\": \"open\""),
        "a new event must be state=open: {doc}"
    );

    let (code, _) = gate.json(
        "POST",
        "/api/events",
        Some(&attendee),
        Some(json!({"title":"nope","starts_at":"2026-10-01T18:00:00Z","capacity":5})),
    );
    assert_eq!(code, 403, "an attendee has no event:write and must be refused");

    let (code, _) = gate.json(
        "POST",
        "/api/events",
        Some(&organizer),
        Some(json!({"starts_at":"2026-10-01T18:00:00Z","capacity":5})),
    );
    assert_eq!(code, 400, "an event with no title is a 400");
    let (code, _) = gate.json(
        "POST",
        "/api/events",
        Some(&organizer),
        Some(json!({"title":"x","starts_at":"2026-10-01T18:00:00Z","capacity":0})),
    );
    assert_eq!(code, 400, "capacity below 1 is a 400");

    let (_, one) = gate.json("GET", &format!("/api/events/{new_id}"), Some(&attendee), None);
    for want in ["\"claimed\"", "\"remaining\""] {
        assert!(
            one.contains(want),
            "GET /api/events/{{id}} must report {want} from quota:meter's peek: {one}"
        );
    }
    assert_eq!(
        field(&one, "remaining"),
        "50",
        "a brand-new event with capacity 50 has 50 remaining"
    );

    // Two separate claims with two separate messages ON PURPOSE: asserting only that the id appeared
    // once reported a GUESS about the cause as a finding and sent a repair to fix a working query.
    let (_, list) = gate.json("GET", "/api/events?state=open", Some(&attendee), None);
    assert!(
        list.contains("\"Wasm Night, moved\"") || list.contains("\"Wasm Night\""),
        "?state=open did not return the open event just created. If other open events came back \
         but not this one, the filter is matching the wrong value — record-store indexes the \
         SERIALISED form, so \"open\" with quotes. Body: {list}"
    );
    assert!(
        list.contains(&new_id),
        "the events list came back without any id on its entries, so nothing in it can be fetched \
         or amended — CONTRACT.md says every entry carries its id. Body: {list}"
    );

    let (code, _) = gate.json("GET", "/api/events/does-not-exist", Some(&attendee), None);
    assert_eq!(code, 404, "an unknown event is a 404");

    let (code, _) = gate.json(
        "PATCH",
        &format!("/api/events/{new_id}"),
        Some(&organizer),
        Some(json!({"title":"Wasm Night, moved"})),
    );
    assert_eq!(code, 200, "the organizer who created the event must be able to PATCH it");

    let (code, _) = gate.json("DELETE", &format!("/api/events/{new_id}"), Some(&organizer), None);
    assert_eq!(code, 204, "DELETE must answer 204");
    let doc = gate.stored("events", &new_id);
    assert!(
        doc.contains("\"state\":\"cancelled\"") || doc.contains("\"state\": \"cancelled\""),
        "DELETE is a SOFT delete — the document stays and state becomes cancelled: {doc}"
    );

    let (_, with) = gate.json(
        "POST",
        "/api/events",
        Some(&organizer),
        Some(json!({
        "title":"Described","starts_at":"2026-10-02T18:00:00Z","capacity":9,
        "description":"An evening about nothing in particular."})),
    );
    let wid = field(&with, "id");
    assert!(
        gate.stored("events", &wid).contains("An evening about nothing in particular."),
        "description was dropped on create: {}",
        gate.stored("events", &wid)
    );
    // Absent, not empty, when not given: "" cannot tell "nobody wrote one" from "somebody cleared it".
    assert!(
        !gate.stored("events", &new_id).contains("\"description\""),
        "an event created without a description must not carry the key"
    );
    let (code, _) = gate.json(
        "PATCH",
        &format!("/api/events/{wid}"),
        Some(&organizer),
        Some(json!({"description": Value::Null})),
    );
    assert_eq!(code, 200, "clearing a description is a PATCH with null");
    assert!(
        !gate.stored("events", &wid).contains("\"description\""),
        "PATCH null must REMOVE the key, not blank it: {}",
        gate.stored("events", &wid)
    );

    let (code, _) = gate.send(
        "POST",
        &format!("/api/events/{wid}/image"),
        Some(&organizer),
        Some(("image/png", PNG.to_vec())),
    );
    assert_eq!(code, 201, "uploading a PNG poster must be 201");

    let (_, content_type, bytes) = gate.bytes(&format!("/api/events/{wid}/image"), None);
    assert_eq!(
        bytes, PNG,
        "the poster did not survive the round trip byte for byte — a body read as a lossy string \
         is not an image"
    );
    assert!(
        content_type.starts_with("image/png"),
        "the poster came back as '{content_type}', not image/png"
    );

    // What may be uploaded is upload-policy's answer, not this component's.
    let (code, _) = gate.send(
        "POST",
        &format!("/api/events/{wid}/image"),
        Some(&organizer),
        Some(("text/plain", b"not an image".to_vec())),
    );
    assert_eq!(code, 415, "a text/plain poster must be refused by upload:policy");

    let (code, _) = gate.json("GET", &format!("/api/events/{new_id}/image"), Some(&attendee), None);
    assert_eq!(code, 404, "an event with no poster is a 404, not an empty 200");
}

// the composition: the gate no single part can pass

/// The whole evening: event opened, place taken, ticket passed on, and the right person walks in.
///
/// Ported from `components/events-domain/e2e.sh`. It lives here rather than in a fifth file because
/// the chain starts with an event and a fifth binary would pay for its own host. What only the
/// composition can prove:
///
///   * `events` reads `remaining` from `quota:meter` and `tickets` must be what moved it: two parts,
///     one counter.
///   * a swap moves a ticket between holders and must NOT free a seat.
///   * `checkin` admits by the holder `swaps` last wrote; a door reading a stale holder lets the wrong
///     person in.
#[test]
fn the_whole_evening_works() {
    let Some(gate) = Gate::start(APP, COMPOSED, CONFIG) else { return };

    let seed = gate.seed();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let subject =
        |who: &str| seed["tokens"][who]["subject"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee, other) = (tok("organizer"), tok("attendee"), tok("other"));

    let (_, e) = gate.post(
        "/api/events",
        Some(&organizer),
        json!({"title":"The Whole Evening","starts_at":"2026-11-05T19:00:00Z","capacity":2}),
    );
    let eid = field(&e, "id");
    assert!(!eid.is_empty(), "events: no event was created: {e}");

    let (_, t) = gate.post(&format!("/api/events/{eid}/tickets"), Some(&attendee), json!({}));
    let tid = field(&t, "id");
    assert!(!tid.is_empty(), "tickets: no ticket was issued: {t}");

    let (_, ev) = gate.get(&format!("/api/events/{eid}"), Some(&organizer));
    let after_claim = field(&ev, "remaining");
    assert_eq!(
        after_claim, "1",
        "capacity 2 minus one claim is 1 remaining, not '{after_claim}' — events reads this \
         from quota:meter, which tickets must be the thing that moved"
    );

    let (_, s) = gate.post("/api/swaps", Some(&attendee), json!({"ticket_id": tid}));
    let sid = field(&s, "id");
    assert!(!sid.is_empty(), "swaps: no offer was created: {s}");
    let (ok, _) = gate.post(&format!("/api/swaps/{sid}/accept"), Some(&other), json!({}));
    assert_eq!(ok, 200, "swaps: the offer could not be accepted (got {ok})");

    let (_, ev) = gate.get(&format!("/api/events/{eid}"), Some(&organizer));
    let still = field(&ev, "remaining");
    assert_eq!(
        still, "1",
        "a swap moved remaining from 1 to '{still}' — the ticket changed hands, the house did \
         not empty a seat"
    );

    let (_, held) = gate.get(&format!("/api/tickets/{tid}"), Some(&other));
    let code = field(&held, "code");
    assert!(
        !code.is_empty(),
        "the acceptor cannot read the ticket they now hold — tickets and swaps disagree about \
         who the holder is: {held}"
    );

    let (_, admitted) = gate.post("/api/checkin", Some(&organizer), json!({"code": code}));
    assert!(
        admitted.contains("checked-in"),
        "checkin: the swapped ticket was not admitted: {admitted}"
    );
    let holder = field(&admitted, "holder");
    let owner = subject("other");
    assert_eq!(
        holder, owner,
        "the door admitted '{holder}' but the ticket belongs to '{owner}' after the swap — \
         checkin is reading a holder that swaps already changed"
    );

    let (c, _) = gate.post("/api/checkin", Some(&organizer), json!({"code": code}));
    assert_eq!(c, 409, "the ticket is spent — a second scan is refused whoever presents it");
}
