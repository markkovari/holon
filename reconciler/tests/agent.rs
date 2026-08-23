//! The agent, end to end: a goal becomes a candidate, and a repair uses the
//! failure it was given.
//!
//! The second part is the whole test. A single call from a goal to a diff is a
//! template; what makes this an agent is that the next attempt is driven by what
//! the gate actually found. Proving that needs a provider whose answer DEPENDS on
//! the failure reaching it — so the scripted provider matches on the failure
//! text, and the repair rule can only fire if it did.
//!
//! Deterministic and free: `mock:provider` declares no egress and no secret, so
//! this could not reach a real model if it tried.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, f) in [
        ("gate", "agent_probe.wasm"),
        ("agent", "agent_writer.wasm"),
        ("llm", "mock_provider.wasm"),
    ] {
        let p = dir.join(f);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn attempt(&self, body: Value) -> Value {
        let r = match self
            .http
            .post(format!("http://127.0.0.1:{}/attempt", self.port))
            .header("host", "agent.acme.test")
            .body(body.to_string())
            .send()
        {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (s, t) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&t).unwrap_or_else(|_| Value::String(format!("HTTP {s}: {t}")))
    }
}

/// The first real attempt, retried — not a separate readiness probe
/// (`Fleet::until`).
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap(),
    };
    fleet.until("an attempt that reaches the model", Duration::from_secs(120), || {
        let r = probe.attempt(json!({
            "text": "make it 42", "writable": ["src/lib.rs"], "seed": 1,
            "context": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }" }],
        }));
        if r["files"].is_array() {
            Ok(())
        } else {
            Err(r.to_string())
        }
    });
    probe
}

fn body_of(v: &Value) -> String {
    v["files"][0]["content"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_goal_becomes_a_candidate_and_a_repair_uses_the_failure() {
    let fleet = Fleet::start_with_secrets("agent", &["fixtures/agent.yaml"], &artifacts(), &[]);
    let probe = wait_for_probe(&fleet);

    let goal = |seed: u64, previous: Value| {
        json!({
            "text": "make it 42", "writable": ["src/lib.rs"], "seed": seed,
            "context": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }" }],
            "previous": previous,
        })
    };

    // --- a first attempt, which is wrong -------------------------------------
    let first = probe.attempt(goal(1, json!([])));
    assert!(first["files"].is_array(), "the agent produced nothing: {first}");
    assert_eq!(body_of(&first), "pub fn answer() -> u32 { 41 }", "{first}");

    // --- branches differ by seed ---------------------------------------------
    // The same question, a different seed: how a generation explores while
    // staying replayable.
    let sibling = probe.attempt(goal(2, json!([])));
    assert_ne!(
        body_of(&sibling),
        body_of(&first),
        "two branches with different seeds produced the same candidate, so a generation \
         would be one branch run twice: {sibling}"
    );

    // --- THE REPAIR ----------------------------------------------------------
    // The gate found the answer was 41. That failure goes back in, and the
    // scripted provider only answers with 42 when it SEES that text — so a
    // correct answer here is proof the failure reached the model rather than the
    // agent re-rolling.
    let repaired =
        probe.attempt(goal(1, json!([{ "id": "the-fix", "detail": "expected 42, found 41" }])));
    assert_eq!(
        body_of(&repaired),
        "pub fn answer() -> u32 { 42 }",
        "the repair did not use the failure — same goal, same seed, and the only thing \
         that changed was what the gate found: {repaired}"
    );
    assert_ne!(
        body_of(&repaired),
        body_of(&first),
        "a repair identical to the attempt is a re-roll"
    );

    // --- an answer that writes where it may not is REFUSED -------------------
    // Not filtered. An answer that touched something it may not is not partially
    // good; it is one nobody should act on.
    let hostile = probe.attempt(json!({
        "text": "not writable here", "writable": ["src/lib.rs"], "seed": 0,
        "context": [],
    }));
    assert_eq!(hostile["error"], json!("unusable-answer"), "must refuse: {hostile}");
    assert!(hostile["detail"].as_str().unwrap_or_default().contains("writable"), "{hostile}");

    // --- an answer with no files is its own failure --------------------------
    // Distinct from the model being down: a caller retries those differently.
    let waffle = probe.attempt(json!({
        "text": "say nothing useful", "writable": ["src/lib.rs"], "seed": 0, "context": [],
    }));
    assert_eq!(waffle["error"], json!("unusable-answer"), "prose is not a candidate: {waffle}");

    // --- a goal with nowhere to write is refused before the model is asked ---
    let nowhere = probe.attempt(json!({ "text": "do something", "writable": [], "seed": 0 }));
    assert_eq!(nowhere["error"], json!("under-specified"), "{nowhere}");

    println!("    41 -> repaired to 42 because the gate said so, and a sibling differed");
}
