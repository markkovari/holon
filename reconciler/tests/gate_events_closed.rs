//! The `events` closed-fixture gate, ported from
//! `components/events-domain/e2e-closed.sh`.
//!
//! Every other events gate runs with `allow-test-routes=1`, so every other events gate
//! proves the app works WITH a fixture route the deployment must never have. This one
//! runs the same artifact with the flag ABSENT and requires that the route is gone —
//! and that the app still works through the front door.
//!
//! It also asks the other half of the same question: with no fixture, who opens the
//! first event? `organizer-emails` is the answer, and it is CONFIG precisely so that a
//! person asking for a role cannot put themselves on it.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::json;

#[test]
fn the_fixture_is_closed_and_signing_up_does_not_make_you_an_organizer() {
    // A per-run address, as the shell used `$$`: two runs against one store would
    // collide on the 409 assertion below.
    let unique = std::process::id();
    let boss_email = format!("boss{unique}@example.test");
    let config = format!("organizer-emails={boss_email}");
    // NOT `allow-test-routes`. That absence is the subject of this gate.
    let Some(gate) = Gate::compose_and_start("events", "events-domain", &[&config]) else { return };

    // --- the fixture is not reachable ---------------------------------------------
    let (c, _) = gate.post("/test/seed", None, json!({}));
    assert_eq!(c, 404, "the fixture must be 404 when allow-test-routes is not set");
    let (c, _) = gate.get("/test/events/anything", None);
    assert_eq!(c, 404, "every /test route goes with it");

    // --- and the front door works --------------------------------------------------
    let email = format!("ada{unique}@example.test");
    let (_, reg) = gate.post("/api/register", None, json!({"email": email, "password":"correct-horse"}));
    let token = field(&reg, "token");
    assert!(!token.is_empty(), "registering did not return a token: {reg}");

    let (c, _) = gate.post("/api/register", None, json!({"email": email, "password":"correct-horse"}));
    assert_eq!(c, 409, "the same email twice is a 409");
    let (c, _) = gate.post("/api/register", None, json!({"email":"nope","password":"correct-horse"}));
    assert_eq!(c, 400, "an address with no @ is a 400");
    let (c, _) = gate.post("/api/register", None,
        json!({"email": format!("x{unique}@example.test"), "password":"short"}));
    assert_eq!(c, 400, "a password under 8 characters is a 400");

    let (_, login) = gate.post("/api/login", None, json!({"email": email, "password":"correct-horse"}));
    assert!(
        login.contains("\"attendee\""),
        "login must report the caller's roles so the SPA knows which screen to draw: {login}"
    );
    let (c, _) = gate.post("/api/login", None, json!({"email": email, "password":"wrong-password"}));
    assert_eq!(c, 401, "a bad password is 401");

    // --- a registered attendee is an ATTENDEE, not an organizer ---------------------
    let (c, _) = gate.post("/api/events", Some(&token),
        json!({"title":"self-promoted","starts_at":"2026-10-01T18:00:00Z","capacity":5}));
    assert_eq!(
        c, 403,
        "signing up must not grant event:write — a person cannot claim a role by asking for one"
    );
    let (c, _) = gate.get("/api/tickets", Some(&token));
    assert_eq!(c, 200, "but they can see their own tickets");

    // --- the deployment can name its first organizer ---------------------------------
    //
    // Without this a fresh box has nobody who may open an event and no organizer to
    // grant the role — a deadlock, not a security property.
    let (_, r) = gate.post("/api/register", None, json!({"email": boss_email, "password":"correct-horse"}));
    let boss = field(&r, "token");
    assert!(!boss.is_empty(), "the named organizer could not register");
    let (c, _) = gate.post("/api/events", Some(&boss),
        json!({"title":"opened by the named organizer","starts_at":"2026-10-01T18:00:00Z","capacity":5}));
    assert_eq!(
        c, 201,
        "an email in organizer-emails must get event:write on registration, or a fresh box has \
         nobody who can open anything"
    );
}
