//! The `moderation:queue` verdict gate, ported from
//! `components/moderation-domain/e2e-verdict.sh`.
//!
//! The interesting assertion is the ORDER: a deny rule that matched wins whatever the
//! model said, and with the policy silent the model's label decides exactly. Both
//! halves need a model — the second maps `allow|flag|block` onto `allowed|flagged|
//! blocked`, so a canned answer would make the mapping untested.
//!
//! Verified against `mlx-community/Qwen3.8-27B-4bit` on csatapaci through
//! `just openai-shim`.

mod gatelib;
use gatelib::{field, Gate, Shim};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

#[test]
fn a_matching_rule_wins_and_a_silent_policy_leaves_it_to_the_model() {
    let Some(shim) = Shim::probe("moderation/verdict") else { return };
    let config = shim.config();
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("moderation", "moderation-domain", &cfg, &[&egress])
    else {
        return;
    };

    let (_, tok) = gate.post("/test/token", None, json!({"subject":"mod"}));
    let t = field(&tok, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    gate.post("/test/rules", None, json!({}));

    let seed = gate.seed();
    let ids: Vec<String> = seed["item_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(ids.len() >= 2, "the fixture produced no items — the scaffold is broken, not the part");
    let (linked, clean) = (ids[0].clone(), ids[1].clone());

    let review = |id: &str| gate.json("POST", &format!("/api/items/{id}/review"), Some(&t), None);

    // --- the rule fires, and it wins ------------------------------------------------
    let (_, raw) = review(&linked);
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    assert_eq!(
        d["policy_rule"], "no-links",
        "the fixture's rule matches this item (its text carries a link) and the decision does not \
         name it. A decision that cannot say what overruled what cannot be audited: {d}"
    );
    assert_eq!(
        d["final"], "blocked",
        "a deny rule that matched means blocked, whatever the model said: {d}"
    );
    let said = d["model_said"].as_str().unwrap_or_default();
    assert!(
        ["allow", "flag", "block"].contains(&said),
        "the decision must record the model's own label, from the three it was given: {d}"
    );
    let conf = d["model_confidence"].as_i64();
    assert!(
        conf.is_some_and(|c| (0..=1000).contains(&c)),
        "confidence is classify's 0..=1000 milli-units, passed through as-is: {:?}",
        d["model_confidence"]
    );
    assert!(
        d["policy_reason"].as_str().is_some_and(|s| !s.is_empty()),
        "the engine's reason belongs in the decision: {d}"
    );
    assert!(
        d["decided_at"].as_str().unwrap_or_default().ends_with('Z'),
        "decided_at must be RFC3339 UTC: {d}"
    );

    let s = parse(&gate.stored("item", &linked));
    assert_eq!(s["state"], "blocked", "the item's state must become the decision's final: {s}");
    assert_eq!(s["decision"]["policy_rule"], "no-links", "the stored decision is incomplete: {s}");

    // --- nothing matches, and the model decides -------------------------------------
    let (_, raw) = review(&clean);
    let d = parse(&raw);
    assert!(
        d["policy_rule"].as_str().unwrap_or_default().is_empty(),
        "no rule in the fixture matches an item with no link, so policy_rule must be empty. A part \
         that reports a rule here is inventing one: {d}"
    );
    let expected: BTreeMap<&str, &str> =
        [("allow", "allowed"), ("flag", "flagged"), ("block", "blocked")].into_iter().collect();
    let said = d["model_said"].as_str().unwrap_or_default();
    let want =
        expected.get(said).unwrap_or_else(|| panic!("the model's label must be recorded: {d}"));
    assert_eq!(
        d["final"], *want,
        "with the policy silent the model decides: it said {said:?}, so final must be {want:?}, not {:?}",
        d["final"]
    );

    // --- deciding twice ---------------------------------------------------------------
    let (code, again) = review(&clean);
    assert_eq!(code, 409, "reviewing an already-decided item must be 409");
    let d = parse(&again);
    assert_eq!(d["error"], "already_decided", "{d}");
    assert!(
        ["allowed", "flagged", "blocked"].contains(&d["final"].as_str().unwrap_or_default()),
        "the 409 must carry the stored final: {d}"
    );
    let (c, _) = review("nope");
    assert_eq!(c, 404, "reviewing an unknown item must be 404");
    let (c, _) = gate.json("POST", &format!("/api/items/{clean}/review"), None, None);
    assert_eq!(c, 401, "reviewing with no bearer must be 401");

    // --- both decisions left the system --------------------------------------------
    //
    // Read off the bus through the router's fixture reader, because `/api/events`
    // belongs to `queue` and is a stub while this part is judged. A decision only this
    // component can see is not a decision anything downstream can act on.
    let evs = parse(&gate.get("/test/events", None).1);
    let mut published: BTreeMap<String, String> = BTreeMap::new();
    for e in evs["events"].as_array().cloned().unwrap_or_default() {
        let p = &e["payload"];
        if let Some(item) = p["item"].as_str() {
            published.insert(item.to_string(), p["final"].as_str().unwrap_or_default().to_string());
        }
    }
    for (item, name) in [(&linked, "the blocked item"), (&clean, "the item the model decided")] {
        assert!(
            published.contains_key(item),
            "{name} was decided but never published to moderation.decided. Everything downstream \
             of this app learns about a decision from the bus. Published: {published:?}"
        );
    }
    assert_eq!(
        published[&linked], "blocked",
        "the published outcome disagrees with the stored one: {:?}",
        published[&linked]
    );
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// The whole moderation API: a rule written, an item submitted, a review decided,
/// and the outcome readable on the bus and in the queue.
///
/// Ported from `components/moderation-domain/e2e.sh`. It lives beside the verdict
/// gate rather than in `gate_moderation.rs` because it needs the same thing that one
/// does — a real model on the shim — and `gate_moderation.rs` is explicitly the file
/// for the two gates that need nothing. Skips loudly without a shim, which is why CI
/// classifies it the same way it classified the shell version.
///
/// What only the composition can prove: the CONTENT here is benign, so a model left
/// to itself would allow it. The rule denies it because of who wrote it. That gap is
/// the join — three parts agreeing on the item's shape, on the attribute names, and
/// on who has the last word. Each part's own gate can see its own half of that and
/// none of them can see the disagreement.
///
/// `author` rather than `has_link` in the rule, deliberately: the fixture's own rule
/// is about links, so a part that depends on the fixture instead of on what was just
/// written through `/api/rules` fails here rather than passing by luck.
#[test]
fn the_whole_moderation_api_works() {
    let Some(shim) = Shim::probe("moderation/whole") else { return };
    let mut config = shim.config();
    config.push("max-attempts=10".into());
    config.push("lockout-window=60".into());
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("moderation", "moderation-domain", &cfg, &[&egress])
    else {
        return;
    };

    for (iface, why) in [
        (
            "ratelimit:guard/limiter",
            "the composed API must still be counting submissions through the limiter",
        ),
        (
            "ai:inference/inference",
            "the composed API must still be asking the model through ai-inference",
        ),
        ("policy:guard/guard", "the composed API must still be deciding through the policy engine"),
        ("event:bus/bus", "the composed API must still be publishing what it decided"),
    ] {
        gatelib::requires_capability("moderation-domain", iface, why);
    }

    let t = field(&gate.post("/test/token", None, json!({"subject":"ada"})).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the parts"
    );

    // --- a rule, written through the part that owns rules ------------------
    let rules = json!({"rules":[{
        "id":"deny-ada","action":"publish","effect":"deny","priority":1,
        "conditions":[{"left":"resource.author","op":"eq","right":"ada"}],
    }]});
    let (got, _) = gate.post("/api/rules", Some(&t), rules);
    assert_eq!(
        got, 204,
        "the queue part did not accept a rule set ({got}), so precedence cannot be judged"
    );

    // --- an item, submitted through the part that owns submission ----------
    let (_, submitted) = gate.post(
        "/api/items",
        Some(&t),
        json!({"text":"a perfectly ordinary message with nothing wrong with it"}),
    );
    let id = field(&submitted, "id");
    assert!(
        !id.is_empty(),
        "the intake part did not accept an item, so nothing else can be judged: {submitted}"
    );

    // --- reviewed through the part that owns review ------------------------
    let (_, body) = gate.post(&format!("/api/items/{id}/review"), Some(&t), json!({}));
    assert!(
        !body.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&body);
    assert!(
        d.get("error").is_none(),
        "the composed API refused a review of an item submitted through the real route. If \
         this is not_found, `intake` (src/intake.rs) and `verdict` (src/verdict.rs) disagree \
         about the `items` collection or its shape. Got: {body}"
    );
    assert_eq!(
        d["policy_rule"], "deny-ada",
        "the rule written through /api/rules did not decide this review. If policy_rule is \
         empty, `queue` (src/queue.rs) wrote rules the engine does not hold, or `verdict` \
         (src/verdict.rs) passed target attributes under different names than the contract's \
         `author`/`has_link`/`model_label`. Got: {body}"
    );
    assert_eq!(
        d["final"], "blocked",
        "a deny rule that matched means blocked whatever the model said: {body}"
    );
    let said = d["model_said"].as_str().unwrap_or_default();
    assert!(
        ["allow", "flag", "block"].contains(&said),
        "the model's own label must be recorded: {body}"
    );

    // --- and the decision is readable through the part that owns the bus ---
    let (_, evs) = gate.get("/api/events", Some(&t));
    let events = parse(&evs)["events"].as_array().cloned().unwrap_or_default();
    let found = events
        .iter()
        .find(|e| e["payload"]["item"].as_str() == Some(id.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "the review was decided but `queue`'s /api/events cannot see it. Either \
                 `verdict` published to a different topic than the contract's \
                 moderation.decided, or `queue` polls a different one. Events seen: {evs}"
            )
        });
    assert_eq!(
        found["payload"]["final"], "blocked",
        "the published outcome disagrees with the decision: {found}"
    );

    // The queue reflects it too: blocked, and no longer pending.
    let ids = |body: &str| -> Vec<String> {
        parse(body)["items"]
            .as_array()
            .map(|a| a.iter().filter_map(|i| i["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let (_, blocked) = gate.get("/api/queue?state=blocked", Some(&t));
    assert!(
        ids(&blocked).contains(&id),
        "a blocked item must appear under state=blocked: {blocked}"
    );
    let (_, pending) = gate.get("/api/queue", Some(&t));
    assert!(!ids(&pending).contains(&id), "a decided item must leave the pending queue: {pending}");
}
