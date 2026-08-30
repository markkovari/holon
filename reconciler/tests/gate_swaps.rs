//! The `swaps` gate: offered, refused to its owner, accepted by another, capacity unmoved.
//!
//! Ported from `components/events-domain/e2e-swaps.sh`. The shell version reached for
//! `python3` twice — once to pull a token out of the fixture, once for the acceptor's
//! SUBJECT — and both are ordinary field reads here.

mod gatelib;
use gatelib::{assert_unauthenticated, field, Gate};
use serde_json::json;

const APP: &str = "events";
const COMPOSED: &str = "events_domain.composed.wasm";
const CONFIG: &[&str] = &["allow-test-routes=1",
    "allowed-types=image/png,image/jpeg,image/webp", "max-size=2097152"];

#[test]
fn swaps_offered_accepted_and_capacity_unmoved() {
    let Some(gate) = Gate::start(APP, COMPOSED, CONFIG) else { return };

    let seed = gate.seed();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee, other) = (tok("organizer"), tok("attendee"), tok("other"));
    let other_subject = seed["tokens"]["other"]["subject"].as_str().unwrap_or_default().to_string();
    let event_id = seed["event_id"].as_str().unwrap_or_default().to_string();

    let (_, t) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&attendee), json!({}));
    let tid = field(&t, "id");
    assert!(!tid.is_empty(), "cannot judge swaps without a ticket — the tickets part answered: {t}");

    let (_, ev) = gate.get(&format!("/api/events/{event_id}"), Some(&organizer));
    let before = field(&ev, "remaining");

    assert_unauthenticated(&gate, "POST", "/api/swaps", Some(json!({"ticket_id": tid})));

    // --- only the holder may offer -------------------------------------------------
    let (c, _) = gate.post("/api/swaps", Some(&other), json!({"ticket_id": tid}));
    assert_eq!(c, 403, "a swap may only be offered by the ticket's holder");

    let (_, s) = gate.post("/api/swaps", Some(&attendee), json!({"ticket_id": tid}));
    let sid = field(&s, "id");
    assert!(!sid.is_empty(), "offering a swap returned no id: {s}");

    let (c, _) = gate.post("/api/swaps", Some(&attendee), json!({"ticket_id": tid}));
    assert_eq!(c, 409, "the same ticket may not carry two open offers");

    let (_, list) = gate.get("/api/swaps", Some(&other));
    assert!(list.contains(&sid), "GET /api/swaps must list the offered swap: {list}");

    // --- you may not accept your own -------------------------------------------------
    let (c, _) = gate.post(&format!("/api/swaps/{sid}/accept"), Some(&attendee), json!({}));
    assert_eq!(c, 403, "the person who offered a swap may not accept it");

    // --- somebody else takes it --------------------------------------------------------
    let (ok, _) = gate.post(&format!("/api/swaps/{sid}/accept"), Some(&other), json!({}));
    assert_eq!(ok, 200, "another attendee must be able to accept the offer");

    let doc = gate.stored("tickets", &tid);
    assert!(
        doc.contains(&other_subject),
        "after an accepted swap the ticket's holder is the acceptor: {doc}"
    );

    let sdoc = gate.stored("swaps", &sid);
    assert!(
        sdoc.contains("\"state\":\"accepted\"") || sdoc.contains("\"state\": \"accepted\""),
        "the swap must become accepted: {sdoc}"
    );

    let (c, _) = gate.post(&format!("/api/swaps/{sid}/accept"), Some(&other), json!({}));
    assert_eq!(c, 409, "an accepted swap cannot be accepted again");

    // --- the house is no fuller ----------------------------------------------------------
    let (_, ev) = gate.get(&format!("/api/events/{event_id}"), Some(&organizer));
    let after = field(&ev, "remaining");
    assert_eq!(
        before, after,
        "a swap changed remaining from {before} to {after} — a swap moves a ticket, it does not \
         release and re-claim a place (see CONTRACT.md)"
    );
}
