//! The `support:desk` courier gate, ported from
//! `components/support-desk-domain/e2e-courier.sh`.
//!
//! This is the gate that needed the harness to run a SERVER, not just a host. Delivery
//! is the whole subject and none of it is observable against a far end that always
//! works: an app that sends inline, one that acks a refusal, and one that retries
//! something already delivered all look identical on the happy path. So the gate runs
//! its own receiver and breaks it on purpose.
//!
//! The shell version writes JSON lines to a temp file and re-reads them, with a
//! footnote about `grep -c` printing 0 AND exiting 1 on an empty file — the empty case
//! being exactly the one that means "nothing was sent". `Sink::arrivals()` is a `Vec`.
//!
//! The sleeps stay. `base-backoff=1` is real time in the outbox, and a retry that has
//! not come due yet is indistinguishable from one that never will.

mod gatelib;
use gatelib::{field, Gate, Shim, Sink};
use serde_json::{json, Value};
use std::time::Duration;

const CRATE: &str = "support-desk-domain";

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

#[test]
fn a_reply_is_delivered_once_retried_on_refusal_and_finally_dead_lettered() {
    let sink = Sink::start();
    let egress = sink.egress();
    // `max-attempts=2 --config base-backoff=1`, as the gate script sets.
    let Some(gate) = Gate::compose_and_start_with_egress(
        "support",
        CRATE,
        &["max-attempts=2", "base-backoff=1"],
        &[&egress],
    ) else {
        return;
    };
    // The readiness probe is itself a delivery, so the log starts clean afterwards.
    sink.forget();

    let (_, tok) = gate.post("/test/token", None, json!({"subject":"agent","tenant":"acme"}));
    let t = field(&tok, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );

    let deliver = || -> Value { parse(&gate.json("POST", "/api/deliver", Some(&t), None).1) };
    let enqueue = |body: &str| {
        gate.post(
            "/test/enqueue",
            None,
            json!({"target": format!("webhook:{}", sink.url()), "body": body}),
        );
    };

    // --- the refusals ---------------------------------------------------------------
    let (c, _) = gate.json("POST", "/api/deliver", None, None);
    assert_eq!(c, 401, "running a delivery pass with no bearer must be 401");
    let (_, ro) =
        gate.post("/test/token", None, json!({"subject":"reader","scopes":["tickets:read"]}));
    let (c, _) = gate.json("POST", "/api/deliver", Some(&field(&ro, "token")), None);
    assert_eq!(c, 403, "delivering needs tickets:deliver — a read-only token must be 403");

    // --- nothing to do is not an error ---------------------------------------------
    let d = deliver();
    for k in ["claimed", "delivered", "failed", "dead"] {
        assert_eq!(d[k], 0, "an empty outbox is a pass that did nothing: {d}");
    }

    // --- a 2xx is delivered, once --------------------------------------------------
    enqueue("the first reply");
    let d = deliver();
    assert_eq!(d["claimed"], 1, "one event was waiting: {d}");
    assert_eq!(
        d["delivered"], 1,
        "the sink answered 200 and this pass did not count a delivery: {d}"
    );
    assert_eq!(d["failed"], 0, "{d}");
    assert_eq!(
        sink.deliveries(),
        1,
        "the sink saw {} arrivals, wanted exactly 1",
        sink.deliveries()
    );

    let arrived = sink.arrivals()[0].body.clone();
    assert!(
        !arrived.trim().is_empty(),
        "a request arrived at the far end with an EMPTY body — the reply itself never left"
    );
    let body: Value = serde_json::from_str(&arrived).unwrap_or_else(|e| {
        panic!("what arrived at the far end is not JSON ({e}): {arrived:.200?}")
    });
    assert!(
        body.to_string().contains("the first reply"),
        "the reply's text did not reach the far end: {body}"
    );

    // A second pass must not deliver it again: an acked event is gone from the outbox.
    let d = deliver();
    assert_eq!(d["claimed"], 0, "a delivered event must not be claimable again: {d}");
    assert_eq!(
        sink.deliveries(),
        1,
        "the reply was delivered twice — the first pass did not ack it"
    );

    // --- a 500 is NOT delivered, and comes back -----------------------------------
    sink.fail();
    enqueue("the second reply");
    let d = deliver();
    assert_eq!(d["claimed"], 1, "one event was waiting: {d}");
    assert_eq!(
        d["delivered"], 0,
        "the far end answered 500 and this pass counted it as delivered. A courier that acks a \
         refusal loses the reply with no trace anywhere: {d}"
    );
    assert_eq!(d["failed"], 1, "a refused send is a failure the outbox has to be told about: {d}");
    assert_eq!(sink.deliveries(), 2, "the refused attempt did not reach the sink at all");

    // It comes back after the backoff, and arrives once the far end recovers.
    sink.repair();
    std::thread::sleep(Duration::from_secs(2));
    let d = deliver();
    assert_eq!(
        d["claimed"], 1,
        "the refused event did not come back after its backoff. If `fail` was never called it \
         is still leased and nothing will ever deliver it: {d}"
    );
    assert_eq!(d["delivered"], 1, "the far end works again and the retry did not land: {d}");
    assert_eq!(sink.deliveries(), 3, "the retry did not reach the sink");

    // --- enough refusals dead-letter it, and a dead letter can be replayed --------
    //
    // The pass that exhausts `max-attempts` must SAY so. The outbox dead-letters on its
    // own whatever the courier reads, so the dead-letter list below would pass a part
    // that never looks at `fail`'s return value — and then nothing in the app ever
    // reports that a reply was abandoned. This is the only place that distinguishes the
    // two.
    sink.fail();
    enqueue("the third reply");
    let mut last = Value::Null;
    for _ in 0..3 {
        last = deliver();
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(
        last["dead"].as_i64().unwrap_or(0) >= 1,
        "max-attempts is spent and this pass reported dead=0. `fail` RETURNS the event's new \
         state and `dead` is the only signal that a reply has been abandoned for good — a part \
         that discards it leaves nothing anywhere to report the loss: {last}"
    );

    let dl = parse(&gate.get("/api/dead-letters", Some(&t)).1);
    let events = dl["events"].as_array().cloned().unwrap_or_default();
    assert!(
        !events.is_empty(),
        "max-attempts is 2 and the far end refused every time; nothing is in the dead letters. \
         Either `fail` was not called or its returned state was ignored."
    );
    let e = &events[0];
    let dead_id = e["id"].as_str().unwrap_or_default().to_string();
    assert!(!dead_id.is_empty(), "a dead letter without its id cannot be replayed: {e}");
    assert!(e["attempts"].as_i64().unwrap_or(0) >= 2, "attempts must be carried: {e}");
    assert!(e["payload"].is_object(), "the payload must come back parsed, not as bytes: {e}");

    sink.repair();
    let (c, _) = gate.json("POST", &format!("/api/dead-letters/{dead_id}/replay"), Some(&t), None);
    assert_eq!(c, 204, "replaying a dead letter must be 204");
    std::thread::sleep(Duration::from_secs(1));
    let d = deliver();
    assert_eq!(d["delivered"], 1, "a replayed reply must be deliverable again: {d}");

    let (c, _) = gate.json("POST", "/api/dead-letters/nope/replay", Some(&t), None);
    assert_eq!(c, 404, "replaying something the outbox does not know must be 404");
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// The whole support desk: a ticket, a drafted reply, and what actually arrives at
/// the customer's endpoint.
///
/// Ported from `components/support-desk-domain/e2e.sh`. It lives beside the courier
/// gate because it needs the same two things that one does — a `Sink` for the far end
/// and a real model on the shim — and because everything it asserts about delivery is
/// already spelled out in this file's header.
///
/// Two things only the composition can prove:
///
///   * the reply must NOT reach the far end before a delivery pass runs. A part that
///     sends inline passes its own gate and defeats the entire point of the app.
///   * what ARRIVES must be the draft that was STORED, field by field. `reply` writes
///     the payload and `courier` reads it, and nothing else in the app would notice
///     if they disagreed about a field name.
///
/// The fields are compared individually and not as a re-serialised blob, and the
/// shell version carries the scar: `json.dumps` escapes non-ASCII by default, so a
/// draft containing an em dash — which a model writes constantly — appears as
/// `—` in the dump and no substring of the original is found in it. That failed
/// a run in which all three parts were correct.
#[test]
fn the_whole_support_desk_works() {
    let Some(shim) = Shim::probe("support/whole") else { return };
    let sink = Sink::start();
    let mut config = shim.config();
    for extra in ["reply-budget=5", "reply-period-secs=3600", "max-attempts=3", "base-backoff=1"] {
        config.push(extra.into());
    }
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let (sink_egress, shim_egress) = (sink.egress(), shim.egress());
    let Some(gate) =
        Gate::compose_and_start_with_egress("support", CRATE, &cfg, &[&sink_egress, &shim_egress])
    else {
        return;
    };
    // The readiness probe is itself a delivery, so the log starts clean afterwards.
    sink.forget();

    for (iface, why) in [
        ("quota:meter/meter", "the composed API must still be metering drafts"),
        ("ai:inference/inference", "the composed API must still be drafting through ai-inference"),
        (
            "outbox:dispatch/queue",
            "the composed API must still be enqueuing rather than sending inline",
        ),
        (
            "notify:dispatch/dispatcher",
            "the composed API must still have something that actually sends",
        ),
        (
            "session:store/store",
            "the composed API must still be checking CSRF against the session that issued it",
        ),
    ] {
        gatelib::requires_capability(CRATE, iface, why);
    }

    let (_, tok) = gate.post("/test/token", None, json!({"subject":"agent","tenant":"acme"}));
    let t = field(&tok, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the parts"
    );
    let (_, sess) = gate.post("/test/session", None, json!({}));
    let (sid, csrf) = (field(&sess, "session"), field(&sess, "csrf"));

    // --- a ticket, through the part that owns tickets ----------------------
    let (_, created) = gate.post(
        "/api/tickets",
        Some(&t),
        json!({
            "subject": "Charged twice this month",
            "body": "There are two invoices dated the same day.",
            "customer": format!("webhook:{}", sink.url()),
        }),
    );
    let id = field(&created, "id");
    assert!(
        !id.is_empty(),
        "the tickets part did not accept a ticket, so nothing else can be judged: {created}"
    );

    // --- a reply, through the part that drafts -----------------------------
    let (_, r) = gate.with_headers(
        "POST",
        &format!("/api/tickets/{id}/reply"),
        Some(&t),
        &[("x-session", sid.as_str()), ("x-csrf", csrf.as_str())],
        None,
    );
    let event = field(&r, "event");
    assert!(
        !event.is_empty(),
        "the reply part drafted nothing usable, so there is nothing to deliver: {r}"
    );
    assert_eq!(
        sink.deliveries(),
        0,
        "the reply reached the far end before any delivery pass ran — the reply part is \
         sending inline, which is the failure this app exists to prevent"
    );

    // --- delivered, through the part that sends ----------------------------
    let (_, body) = gate.post("/api/deliver", Some(&t), json!({}));
    assert!(
        !body.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&body);
    assert_eq!(
        d["claimed"], 1,
        "the courier claimed nothing. If the reply part enqueued under a different topic \
         than the contract's support.reply, the two never meet: {body}"
    );
    assert_eq!(
        d["delivered"], 1,
        "the sink answered 200 and this was not counted as delivered: {body}"
    );
    assert_eq!(
        sink.deliveries(),
        1,
        "the reply did not reach the far end exactly once (saw {} arrivals)",
        sink.deliveries()
    );

    // --- THE join assertion ------------------------------------------------
    //
    // What arrived is what the customer should read, which means the two parts agreed
    // on every field of a payload neither of them shows anyone.
    let arrivals = sink.arrivals();
    let arrival = arrivals.first().expect("nothing arrived at the far end at all");
    let arrived = arrival.body.trim();
    assert!(
        !arrived.is_empty(),
        "a request arrived at the far end with an EMPTY body — the reply never left"
    );
    let payload: Value = serde_json::from_str(arrived).unwrap_or_else(|e| {
        panic!("what arrived at the far end is not JSON ({e}): {:.200}", arrived)
    });
    assert!(payload.is_object(), "what arrived is not a JSON object: {payload}");

    let (_, stored) = gate.get(&format!("/test/ticket/{id}"), None);
    let ticket = parse(&stored);
    let drafted = ticket["reply"]["text"].as_str().unwrap_or_default().trim().to_string();
    assert!(!drafted.is_empty(), "the ticket has no stored draft to compare against: {stored}");

    let arrived_body = payload["body"].as_str().unwrap_or_default().trim();
    assert_eq!(
        arrived_body, drafted,
        "what arrived at the customer's endpoint is not the draft that was stored. `reply` \
         writes the payload and `courier` reads it, and nothing else in the app would notice \
         if they disagreed about a field name.\n  stored:  {:.160?}\n  arrived: {:.160?}",
        drafted, arrived_body
    );
    assert!(
        payload["subject"].as_str().unwrap_or_default().contains("Charged twice"),
        "the subject did not survive the trip: {:?}",
        payload["subject"]
    );
    assert!(
        !payload["ticket"].as_str().unwrap_or_default().is_empty(),
        "the payload must carry the ticket it answers: {payload}"
    );
}
