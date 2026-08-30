//! The `docsearch:agent` answer gate, ported from
//! `components/doc-search-domain/e2e-answer.sh`.
//!
//! One of the five that genuinely needs a model. Its assertions are about the app —
//! step-up before an answer, a budget of one, a cache checked BEFORE the meter — but
//! one of them is about the answer itself: the seeded document says the reconciler
//! polls every three seconds, and a canned sentence mentions none of it. Pointing this
//! at `mock-provider` would prove the app plumbed a string through.
//!
//! So the requirement is ported rather than removed: `Shim::probe` skips loudly when
//! nothing is listening, exactly as `gate_shim_config` does.
//!
//! Verified against `mlx-community/Qwen3.8-27B-4bit` on csatapaci through
//! `just openai-shim`.

mod gatelib;
use gatelib::{field, Gate, Shim};
use serde_json::{json, Value};
use std::time::Instant;

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

#[test]
fn an_answer_is_paid_for_once_cached_forever_and_gated_by_step_up() {
    let Some(shim) = Shim::probe("docsearch/answer") else { return };
    // A budget of one is the whole point: it is what makes "cache before meter"
    // observable at all.
    let mut config: Vec<String> = vec![
        "answer-budget=1".into(),
        "answer-period-secs=3600".into(),
        "answer-cache-ttl-secs=300".into(),
    ];
    config.extend(shim.config());
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) = Gate::compose_and_start_with_egress(
        "docsearch", "doc-search-domain", &cfg, &[&egress],
    ) else {
        return;
    };

    gate.seed();
    let (_, tok) = gate.post("/test/token", None, json!({"subject":"ada"}));
    let t = field(&tok, "token");
    assert!(!t.is_empty(), "POST /test/token returned no token — the scaffold is broken, not the part");
    let ask = |q: &str| gate.post("/api/answer", Some(&t), json!({ "question": q }));

    // --- no step-up, no answer ------------------------------------------------------
    //
    // Before the fixture marks anyone: the first check must be the one that costs
    // nothing.
    let (c, _) = ask("How often does the reconciler poll inventory?");
    assert_eq!(c, 403, "a session that has not stepped up must be 403 step_up_required");

    gate.post("/test/stepup", None, json!({"subject":"ada"}));

    // --- the first real question ----------------------------------------------------
    const Q: &str = "How long does the reconciler wait between inventory polls?";
    let (_, first) = ask(Q);
    assert!(!first.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    let d = parse(&first);
    let a = d["answer"].as_str().unwrap_or_default().trim().to_string();
    assert!((5..=2000).contains(&a.len()), "no usable answer, got {} chars: {a:?}", a.len());
    assert_eq!(d["cached"], false, "the first answer to a question cannot be cached: {d}");
    assert!(
        d["sources"].as_array().is_some_and(|s| !s.is_empty()),
        "an answer must name the documents it came from: {d}"
    );
    assert_eq!(d["remaining"], 0, "one answer out of a budget of one leaves 0 remaining: {d}");
    // About THIS question, and not a slice of the source: the seeded document says the
    // reconciler polls every three seconds, and a canned sentence mentions none of it.
    let low = a.to_lowercase();
    assert!(
        ["three", "3 second", "3-second", "second"].iter().any(|w| low.contains(w)),
        "the answer says nothing about the interval it was asked for: {a:?}"
    );

    // --- the same question again: free ---------------------------------------------
    let start = Instant::now();
    let (_, second) = ask(Q);
    let elapsed = start.elapsed().as_secs_f64();
    let d = parse(&second);
    assert_eq!(d["cached"], true, "the second identical question must be served from the cache: {d}");
    assert_eq!(d["answer"], parse(&first)["answer"], "a cache hit must return the answer that was cached");
    assert_eq!(d["remaining"], 0, "a cache hit spends nothing, so remaining is unchanged: {d}");
    // A real model call through the shim takes seconds. A cache hit cannot.
    assert!(elapsed < 4.0, "the second answer took {elapsed:.1}s — that is a model call, not a cache hit");

    // --- a question the library cannot support: also free --------------------------
    let (c, _) = ask("What temperature should I proof sourdough at?");
    assert_eq!(c, 404, "a question with no matching sources must be 404 no_sources");

    // --- the budget is gone, and the paid-for answer is still served ---------------
    const OTHER: &str = "Why does raising the per-instance memory ceiling cost address space?";
    let (_, new) = ask(OTHER);
    let d = parse(&new);
    assert_eq!(d["error"], "budget_exhausted", "a second distinct question on a budget of one must be refused: {d}");
    assert!(d["retry_after"].as_i64().unwrap_or(0) > 0, "a refusal must say how long to wait: {d}");
    let (c, _) = ask(OTHER);
    assert_eq!(c, 429, "a refused question must be 429");

    // THE ordering check: an answer already paid for survives the budget running out.
    // If the meter is consulted before the cache, this is a 429 and the part has the
    // order wrong.
    let (_, again) = ask(Q);
    let d = parse(&again);
    assert!(
        d["cached"] == true && d["answer"].as_str().is_some_and(|s| !s.is_empty()),
        "an answer already paid for stopped being served once the budget ran out — the cache \
         must be checked BEFORE the meter: {d}"
    );
}
