//! The two portable `moderation:queue` gates, ported from
//! `components/moderation-domain/e2e-*.sh`. `e2e-verdict.sh` wants a model on :8787
//! and stays a shell gate.
//!
//! Assertions and failure sentences unchanged — ADR-0088 makes a gate's output the
//! next prompt a repair reads.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const CRATE: &str = "moderation-domain";

/// Per-GATE config, not per-app: the shell gates set `GATE_CONFIG` in the script
/// rather than the lib, and `e2e-intake.sh` sets the limiter to three attempts. A
/// harness that only read the libs missed it, and the fourth submission — which the
/// gate exists to see refused — was accepted.
fn start(config: &[&str]) -> Option<Gate> {
    Gate::compose_and_start("moderation", CRATE, config)
}
fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut b = json!({ "subject": subject });
    if let Some(s) = scopes {
        b["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, b).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}

#[test]
fn intake_stores_the_principal_and_honours_the_limit() {
    let Some(gate) = start(&["max-attempts=3", "lockout-window=60"]) else { return };
    let w = token(&gate, "ada", None);
    let item = json!({"text":"has anyone tried the new deploy flow?"});

    // --- the refusals, none of which spends an attempt -----------------------------
    let (c, _) = gate.post("/api/items", None, item.clone());
    assert_eq!(c, 401, "submitting with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["items:read"])));
    let (c, _) = gate.post("/api/items", Some(&ro), item.clone());
    assert_eq!(c, 403, "a token with only items:read must be 403 on a submission");
    let (c, _) = gate.post("/api/items", Some(&w), json!({"text":""}));
    assert_eq!(c, 400, "empty text must be 400 invalid_item");

    // --- an item goes in ------------------------------------------------------------
    let (_, created) = gate.post("/api/items", Some(&w), item);
    let id = field(&created, "id");
    assert!(!id.is_empty(), "POST /api/items returned no id");

    let raw = gate.stored("item", &id);
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    assert_eq!(d["state"], "pending", "a new item is pending: {d}");
    assert_eq!(d["author"], "ada", "author must be the principal's subject, not {:?}", d["author"]);
    assert!(
        d.get("decision").is_none(),
        "intake must not invent a decision — that is the verdict part's job"
    );
    assert!(
        d["submitted_at"].as_str().unwrap_or_default().ends_with('Z'),
        "submitted_at must be RFC3339 UTC: {:?}",
        d["submitted_at"]
    );

    let (_, read) = gate.get(&format!("/api/items/{id}"), Some(&w));
    assert_eq!(
        parse(&read)["text"],
        "has anyone tried the new deploy flow?",
        "GET /api/items/{{id}} did not answer the stored item: {read}"
    );
    let (c, _) = gate.get("/api/items/nope", Some(&w));
    assert_eq!(c, 404, "an unknown item id must be 404");

    // --- and the limit, which counts what was accepted -----------------------------
    let burst = token(&gate, "burst", None);
    for i in 1..=3 {
        let (c, _) = gate.post("/api/items", Some(&burst), json!({"text": format!("burst {i}")}));
        assert_eq!(c, 201, "submission {i} of 3 within the limit must be accepted");
    }
    let (_, locked) = gate.post("/api/items", Some(&burst), json!({"text":"burst 4"}));
    let d = parse(&locked);
    assert_eq!(
        d["error"], "rate_limited",
        "past the limit the part must refuse and say how long to wait: {d}"
    );
    assert!(
        d["retry_after"].as_i64().unwrap_or(0) > 0,
        "retry_after must be the limiter's seconds: {d}"
    );

    let (c, _) = gate.post("/api/items", Some(&w), json!({"text":"still fine"}));
    assert_eq!(c, 201, "locking out one subject must not lock out another");
}

#[test]
fn queue_reads_the_engine_and_does_not_consume_the_bus() {
    let Some(gate) = start(&[]) else { return };
    let t = token(&gate, "mod", None);

    let seed = gate.seed();
    let seeded: Vec<String> = seed["item_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        !seeded.is_empty(),
        "the fixture produced no items — the scaffold is broken, not the part"
    );

    // --- the refusals ---------------------------------------------------------------
    let (c, _) = gate.get("/api/queue", None);
    assert_eq!(c, 401, "reading the queue with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["items:read"])));
    let (c, _) = gate.post("/api/rules", Some(&ro), json!({"rules":[]}));
    assert_eq!(c, 403, "writing rules needs items:moderate — a read-only token must be 403");

    // --- what is waiting ------------------------------------------------------------
    let (_, q) = gate.get("/api/queue", Some(&t));
    assert!(
        !q.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let items = parse(&q)["items"].as_array().cloned().unwrap_or_default();
    assert!(items.len() >= 2, "two items were seeded and the queue shows {q}");
    let ids: Vec<&str> = items.iter().filter_map(|i| i["id"].as_str()).collect();
    for s in &seeded {
        assert!(
            ids.contains(&s.as_str()),
            "a pending item is missing from the queue: {s} not in {ids:?}"
        );
    }
    for i in &items {
        assert_eq!(i["state"], "pending", "the default queue is the pending one: {i}");
        assert!(
            i["id"].as_str().is_some_and(|s| !s.is_empty()),
            "an item without its id cannot be reviewed: {i}"
        );
    }
    let stamps: Vec<&str> = items.iter().filter_map(|i| i["submitted_at"].as_str()).collect();
    let mut sorted = stamps.clone();
    sorted.sort();
    assert_eq!(stamps, sorted, "a queue is oldest first, not newest: {stamps:?}");

    let (_, blocked) = gate.get("/api/queue?state=blocked", Some(&t));
    assert_eq!(
        parse(&blocked)["items"],
        json!([]),
        "nothing is blocked yet and the queue said otherwise"
    );

    // --- the rules go in through the engine, and come back out of it ---------------
    let rules = json!({"rules":[{"id":"deny-shouting","action":"publish","effect":"deny","priority":5,
        "conditions":[{"left":"resource.model_label","op":"eq","right":"block"}]}]});
    let (c, _) = gate.post("/api/rules", Some(&t), rules);
    assert_eq!(c, 204, "writing a valid rule set must be 204");

    let back = parse(&gate.get("/api/rules", Some(&t)).1);
    let rs = back["rules"].as_array().cloned().unwrap_or_default();
    assert_eq!(rs.len(), 1, "one rule was written and {rs:?} came back");
    let r = &rs[0];
    assert_eq!(r["id"], "deny-shouting", "the rules did not come back as they were written: {r}");
    assert_eq!(r["effect"], "deny", "{r}");
    assert_eq!(r["priority"], 5, "{r}");
    let conds = r["conditions"].as_array().cloned().unwrap_or_default();
    assert!(
        conds.first().is_some_and(|c| c["left"] == "resource.model_label" && c["right"] == "block"),
        "{r}"
    );

    // The fixture writes a DIFFERENT rule set straight through the engine. A part
    // answering from its own copy still says `deny-shouting`; a part reading the engine
    // says `no-links`.
    gate.post("/test/rules", None, json!({}));
    let back = parse(&gate.get("/api/rules", Some(&t)).1);
    let ids: Vec<&str> = back["rules"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["id"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        ids,
        ["no-links"],
        "something else replaced the rules through policy:guard and this route still reports \
         {ids:?}. The rules a reviewer reads must be the rules the engine holds, or they are \
         not the rules any decision used."
    );

    // An invalid rule is refused here, because a rule the engine rejects later is a rule
    // nobody wrote down.
    for (body, why) in [
        (
            json!({"rules":[{"id":"x","action":"publish","effect":"maybe","priority":1,"conditions":[]}]}),
            "an unknown effect must be 400 invalid_rule",
        ),
        (
            json!({"rules":[{"id":"x","action":"publish","effect":"deny","priority":1,
                "conditions":[{"left":"a","op":"sideways","right":"b"}]}]}),
            "an unknown op must be 400 invalid_rule",
        ),
    ] {
        let (c, _) = gate.post("/api/rules", Some(&t), body);
        assert_eq!(c, 400, "{why}");
    }

    // --- what has left the system --------------------------------------------------
    //
    // `verdict` is a stub here, so nothing has published a decision. The bus is empty
    // and this route must say so — the check is that it READS the bus rather than
    // inventing a list, and that reading twice gives the same answer, which an `ack`
    // would not.
    let (_, one) = gate.get("/api/events", Some(&t));
    assert!(
        !one.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    assert_eq!(
        parse(&one)["events"],
        json!([]),
        "nothing has been published and this route answered {one}"
    );
    let (_, two) = gate.get("/api/events", Some(&t));
    assert_eq!(
        one, two,
        "reading the events twice gave different answers — a read that consumes is not a read (do not ack)"
    );
}
