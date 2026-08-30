//! The `checkin` gate: scanned once, refused twice with the machine's own state.
//!
//! Ported from `components/events-domain/e2e-checkin.sh`. Assertions and failure
//! sentences unchanged — ADR-0088 makes a gate's output the next prompt.

mod gatelib;
use gatelib::{assert_unauthenticated, field, requires_capability, Gate};
use serde_json::json;

const APP: &str = "events";
const COMPOSED: &str = "events_domain.composed.wasm";
const CONFIG: &[&str] = &["allowed-types=image/png,image/jpeg,image/webp", "max-size=2097152"];

#[test]
fn checkin_scanned_once_and_refused_twice() {
    let Some(gate) = Gate::start(APP, COMPOSED, CONFIG) else { return };

    requires_capability("events-domain", "fsm:workflow/engine",
        "the ticket lifecycle is a DEFINITION registered with fsm-workflow, not a ladder of string \
         comparisons — and the refusal to check a ticket in twice comes from the machine, carrying \
         the current state (see CONTRACT.md)");

    let seed = gate.seed();
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee) = (tok("organizer"), tok("attendee"));
    let event_id = seed["event_id"].as_str().unwrap_or_default().to_string();

    // A ticket to scan. `tickets` owns claiming and may still be a stub, so the
    // fixture's event is used through the same route and the gate says so clearly
    // rather than blaming this part for it.
    let (_, t) = gate.post(&format!("/api/events/{event_id}/tickets"), Some(&attendee), json!({}));
    let (code, tid) = (field(&t, "code"), field(&t, "id"));
    assert!(!code.is_empty(), "cannot judge check-in without a ticket — the tickets part answered: {t}");

    assert_unauthenticated(&gate, "POST", "/api/checkin", Some(json!({"code": code})));

    // --- an attendee may not scan ----------------------------------------------------
    let (c, _) = gate.post("/api/checkin", Some(&attendee), json!({"code": code}));
    assert_eq!(c, 403, "an attendee has no checkin:write");

    // --- the organizer scans ----------------------------------------------------------
    let (_, r#in) = gate.post("/api/checkin", Some(&organizer), json!({"code": code}));
    for want in ["\"ticket_id\"", "\"event_id\"", "\"holder\"", "\"state\""] {
        assert!(r#in.contains(want), "the check-in reply is missing {want}: {}", r#in);
    }
    assert!(r#in.contains("checked-in"), "after a scan the state is checked-in: {}", r#in);

    // The document must move too, or GET /api/tickets/{id} disagrees with the machine.
    let doc = gate.stored("tickets", &tid);
    assert!(
        doc.contains("checked-in"),
        "the ticket DOCUMENT still does not say checked-in — move both (see CONTRACT.md): {doc}"
    );

    // --- twice is a 409 that names the state -------------------------------------------
    let (c, again) = gate.post("/api/checkin", Some(&organizer), json!({"code": code}));
    assert!(again.contains("already_checked_in"), "a second scan must be 409 already_checked_in: {again}");
    assert!(
        again.contains("checked-in"),
        "the 409 must carry the CURRENT state, which fsm's IllegalTransition already gives you: {again}"
    );
    assert_eq!(c, 409, "a repeat scan is 409");

    // --- an unknown code ----------------------------------------------------------------
    let (c, _) = gate.post("/api/checkin", Some(&organizer), json!({"code":"not-a-real-code-at-all"}));
    assert_eq!(c, 404, "an unknown code is 404 no_such_ticket, not 500 and not 200");
}
