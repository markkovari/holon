//! The `support:desk` reply gate, ported from
//! `components/support-desk-domain/e2e-reply.sh`.
//!
//! Needs BOTH a model and a sink, and the sink is there to prove a negative: the reply
//! is drafted, stored and queued, and nothing must reach the far end. The courier is a
//! stub in this run, so anything arriving at the sink means this part sent it inline —
//! which is the failure the whole app exists to prevent, and which every other check
//! here would pass while doing.
//!
//! Verified against `mlx-community/Qwen3.8-27B-4bit` on csatapaci through
//! `just openai-shim`.

mod gatelib;
use gatelib::{field, Gate, Shim, Sink};
use serde_json::{json, Value};

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

#[test]
fn a_reply_is_drafted_queued_and_not_sent_by_this_part() {
    let Some(shim) = Shim::probe("support-desk/reply") else { return };
    let sink = Sink::start();

    let mut config: Vec<String> = vec!["reply-budget=1".into(), "reply-period-secs=3600".into()];
    config.extend(shim.config());
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let (shim_egress, sink_egress) = (shim.egress(), sink.egress());
    let Some(gate) = Gate::compose_and_start_with_egress(
        "support",
        "support-desk-domain",
        &cfg,
        &[&shim_egress, &sink_egress],
    ) else {
        return;
    };
    sink.forget();

    let (_, tok) = gate.post("/test/token", None, json!({"subject":"agent","tenant":"acme"}));
    let t = field(&tok, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );

    let (_, sess) = gate.post("/test/session", None, json!({}));
    let (sid, csrf) = (field(&sess, "session"), field(&sess, "csrf"));
    assert!(
        !sid.is_empty() && !csrf.is_empty(),
        "the fixture could not open a session — the scaffold is broken, not the part"
    );

    let (_, seeded) =
        gate.post("/test/seed", None, json!({"target": format!("webhook:{}", sink.url())}));
    let ids: Vec<String> = parse(&seeded)["ticket_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        ids.len() >= 2,
        "the fixture produced no tickets — the scaffold is broken, not the part"
    );
    let (one, two) = (ids[0].clone(), ids[1].clone());

    let reply = |id: &str| {
        gate.with_headers(
            "POST",
            &format!("/api/tickets/{id}/reply"),
            Some(&t),
            &[("x-session", &sid), ("x-csrf", &csrf)],
            None,
        )
    };

    // --- CSRF comes first, and costs nothing ---------------------------------------
    let (c, _) = gate.json("POST", &format!("/api/tickets/{one}/reply"), Some(&t), None);
    assert_eq!(c, 403, "a reply with no session or csrf header must be 403 csrf_required");
    let (c, _) = gate.with_headers(
        "POST",
        &format!("/api/tickets/{one}/reply"),
        Some(&t),
        &[("x-session", &sid), ("x-csrf", "wrong")],
        None,
    );
    assert_eq!(c, 403, "a reply with the wrong csrf token must be 403 csrf_invalid");
    let (c, _) = gate.with_headers(
        "POST",
        &format!("/api/tickets/{one}/reply"),
        Some(&t),
        &[("x-session", "nope"), ("x-csrf", &csrf)],
        None,
    );
    assert_eq!(c, 403, "a reply against a session that does not exist must be 403 session_expired");
    let (c, _) = gate.with_headers(
        "POST",
        &format!("/api/tickets/{one}/reply"),
        None,
        &[("x-session", &sid), ("x-csrf", &csrf)],
        None,
    );
    assert_eq!(c, 401, "no bearer at all is 401, before any csrf talk");
    let (c, _) = reply("nope");
    assert_eq!(c, 404, "replying to an unknown ticket must be 404");

    // --- the draft -----------------------------------------------------------------
    //
    // The status matters as much as the body: 200 tells a customer's agent the reply has
    // been sent when nothing has left the building yet.
    let (code, r) = reply(&one);
    assert_eq!(code, 202, "a drafted reply is 202 Accepted — nothing has been delivered yet");
    assert!(
        !r.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&r);
    assert!(
        d["event"].as_str().is_some_and(|s| !s.is_empty()),
        "the answer must name the outbox event the reply is waiting in: {d}"
    );
    assert_eq!(d["remaining"], 0, "one draft out of a budget of one leaves 0 remaining: {d}");

    let (c, _) = reply(&two);
    assert_eq!(c, 429, "the second draft on a budget of one must be 429 budget_exhausted");

    // The stored ticket, and the draft that the model actually wrote.
    let stored = gate.stored("ticket", &one);
    assert!(
        !stored.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&stored);
    assert_eq!(d["state"], "answered", "a ticket with a draft is answered: {d}");
    let rep = &d["reply"];
    assert!(rep.is_object(), "the ticket has no reply block: {d}");
    let text = rep["text"].as_str().unwrap_or_default().trim().to_string();
    assert!(text.len() >= 20, "no usable draft was stored: {text:?}");
    assert!(
        rep["event"].as_str().is_some_and(|s| !s.is_empty()),
        "the stored reply must name its outbox event: {rep}"
    );
    assert!(
        rep["drafted_at"].as_str().unwrap_or_default().ends_with('Z'),
        "drafted_at must be RFC3339 UTC: {rep}"
    );
    // About THIS ticket, and not a slice of it: the seeded ticket is about being charged
    // for the wrong plan, and a canned sentence mentions none of it.
    let low = text.to_lowercase();
    assert!(
        ["plan", "invoice", "charg", "team", "pro", "billing"].iter().any(|w| low.contains(w)),
        "the draft is not about the ticket it answers: {text:?}"
    );
    assert!(
        !d["body"].as_str().unwrap_or_default().contains(&text),
        "the draft is a verbatim slice of the customer's message"
    );

    // --- and nothing was sent ------------------------------------------------------
    assert_eq!(
        sink.deliveries(),
        0,
        "the reply reached the far end without any delivery pass — this part is sending inline. \
         When the far end is down that reply is lost, the budget was already spent, and nothing \
         records that it existed. Enqueue it (outbox:dispatch) and let the courier deliver it."
    );

    // --- and a second reply to the same ticket is a conflict -----------------------
    let (c, _) = reply(&one);
    assert_eq!(c, 409, "a second reply to an already-answered ticket must be 409");
}
