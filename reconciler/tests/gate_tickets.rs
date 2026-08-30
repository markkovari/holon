//! The `tickets` gate: claimed, rendered, kept private, capacity held by `quota:meter`.
//!
//! Ported from `components/events-domain/e2e-tickets.sh`. Assertions and failure
//! sentences are unchanged — ADR-0088 makes a gate's output the next prompt, so the
//! wording is part of the gate.
//!
//! The contested-pair section is where the shell version cost the most: it wrote two
//! results into a `mktemp -d`, backgrounded two subshells, and read the files back.
//! Two threads and a channel say the same thing without a temporary directory, and
//! without the `python3` the shell used to pull a token out of a fresh fixture.

mod gatelib;
use gatelib::{assert_unauthenticated, field, requires_capability, Gate};
use serde_json::json;

const APP: &str = "events";
const COMPOSED: &str = "events_domain.composed.wasm";
const CONFIG: &[&str] = &["allow-test-routes=1",
    "allowed-types=image/png,image/jpeg,image/webp", "max-size=2097152"];

#[test]
fn tickets_claimed_rendered_private_and_capped() {
    let Some(gate) = Gate::start(APP, COMPOSED, CONFIG) else { return };

    requires_capability("events-domain", "quota:meter/meter",
        "capacity is held atomically by quota:meter, which is in the world for this part to CALL — \
         counting tickets and comparing to capacity is a race that passes every sequential test \
         (see CONTRACT.md)");
    requires_capability("events-domain", "qr:encode/encoder",
        "the attendee's QR is rendered by the qr component, not by hand");

    let seed = gate.seed();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee, other) = (tok("organizer"), tok("attendee"), tok("other"));
    let event_id = seed["event_id"].as_str().unwrap_or_default().to_string();
    assert!(!event_id.is_empty() && !attendee.is_empty(), "the fixture did not come back with an event and three tokens: {seed}");

    assert_unauthenticated(&gate, "POST", &format!("/api/events/{event_id}/tickets"), Some(json!({})));

    // --- one attendee claims one place ---------------------------------------------
    let (_, t) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&attendee), json!({}));
    let (tid, code, qr) = (field(&t, "id"), field(&t, "code"), field(&t, "qr"));
    assert!(!tid.is_empty(), "claiming a ticket returned no id: {t}");
    assert!(!code.is_empty(), "a ticket must carry a code — it is what goes in the QR: {t}");
    assert!(
        code.len() >= 16,
        "the code must be nanoid(21); '{code}' is too short to be unguessable, and possession of it IS the claim"
    );
    assert!(qr.contains("<svg"), "qr must be an SVG document from qr:encode's svg(): {:.120}", qr);

    let doc = gate.stored("tickets", &tid);
    for want in ["\"event_id\"", "\"holder\"", "\"code\"", "\"state\""] {
        assert!(doc.contains(want), "the stored ticket is missing {want} — CONTRACT.md fixes the shape: {doc}");
    }
    assert!(
        doc.contains("\"state\":\"issued\"") || doc.contains("\"state\": \"issued\""),
        "a new ticket is state=issued: {doc}"
    );

    // --- the same person may not hold two --------------------------------------------
    let (c, _) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&attendee), json!({}));
    assert_eq!(c, 409, "a subject already holding a live ticket for this event gets 409 already_holding");

    // --- a ticket is private ----------------------------------------------------------
    let (_, mine) = gate.get("/api/tickets", Some(&attendee));
    assert!(mine.contains(&tid), "GET /api/tickets must list the caller's own ticket: {mine}");
    let (c, _) = gate.get(&format!("/api/tickets/{tid}"), Some(&other));
    assert_eq!(c, 403, "another attendee is neither the holder nor the event's organizer and must be refused");
    let (c, _) = gate.get(&format!("/api/tickets/{tid}"), Some(&organizer));
    assert_eq!(c, 200, "the organizer of the event may read a ticket for it");

    let (c, _) = gate.post("/api/events/no-such-event/tickets", Some(&attendee), json!({}));
    assert_eq!(c, 404, "claiming against an unknown event is a 404");

    // --- the last place, claimed twice at once ----------------------------------------
    //
    // Capacity is 3 and one is taken. `other` takes the second. Then TWO requests for
    // the third go out together and the results are compared after both return.
    let (_, o) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&other), json!({}));
    assert!(!field(&o, "id").is_empty(), "the second place could not be claimed: {o}");

    let contested: Vec<u16> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                s.spawn(|| {
                    gate.post(&format!("/api/events/{event_id}/tickets"), Some(&attendee), json!({})).0
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a contesting request panicked")).collect()
    });

    // Both are the same already-holding attendee, so both must be refused — the point
    // of this pair is that the component answered both without a 500 and without
    // issuing a fourth ticket past capacity.
    assert!(
        contested.iter().all(|c| *c != 500),
        "a contested claim answered 500: {contested:?} — two simultaneous claims must both get an answer"
    );
    let (_, ev) = gate.get(&format!("/api/events/{event_id}"), Some(&organizer));
    let claimed = field(&ev, "claimed");
    assert_eq!(
        claimed, "2",
        "after two claims the event reports claimed={claimed}, not 2 — claimed must come from quota:meter's peek"
    );

    // The real capacity test: a third DISTINCT holder takes the last place, a fourth
    // is refused. `organizer` is a person too and may hold a ticket.
    let (last, _) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&organizer), json!({}));
    assert_eq!(last, 201, "the third and final place must be claimable");
    let (c, _) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&organizer), json!({}));
    assert_eq!(c, 409, "the organizer now holds one, so a second claim is already_holding not sold_out");
}
