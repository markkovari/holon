//! A scripted provider behind `llm:inference`: correctness without a bill.
//!
//! Testing a graph loop against a real model is impossible in the strict sense —
//! not merely expensive. A swarm test asserts things like "twelve branches
//! explore, branch seven wins, a pull request opens against the right base", and
//! a non-deterministic oracle makes that assertion meaningless: the run that
//! passes and the run that fails differ for reasons the test cannot see. Money is
//! the second problem. Determinism is the first.
//!
//! So the same probe used against `openai-provider` in `inference.rs` is pointed
//! at `mock-provider` instead, with nothing else changed. That is the swap point
//! working: the caller is identical and cannot tell.
//!
//! The strongest property here is not in any assertion below — it is in the
//! fixture. `mock-provider` declares no `wasi:http/outgoing-handler` and no
//! secret, so this test COULD NOT reach a provider or authenticate to one if it
//! tried. That is a guarantee from the manifest, not from remembering to point a
//! base URL somewhere harmless.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [("gate", "llm_probe.wasm"), ("llm", "mock_provider.wasm")] {
        let p = dir.join(file);
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
    fn get(&self, path: &str) -> Value {
        let r = match self
            .http
            .get(format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "llm.acme.test")
            .send()
        {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }

    /// One branch's attempt at the shared question.
    fn ask(&self, question: &str, seed: u64) -> Value {
        self.get(&format!("/chat?q={}&seed={seed}", question.replace(' ', "+")))
    }
}

fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut last = Value::Null;
    while std::time::Instant::now() < deadline {
        // Readiness crosses the LINK, not just the gate: `/describe` calls the
        // provider. The root route touches no capability and would answer before
        // the link is usable.
        let r = probe.get("/describe");
        if r["provider"].is_string() {
            return probe;
        }
        last = r;
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!(
        "the llm app never reached its provider — last answer {last}\n--- node ---\n{}\n\
         --- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
}

#[test]
fn a_scripted_provider_makes_a_swarm_reproducible_and_free() {
    let fleet = Fleet::start_with_secrets("mockllm", &["fixtures/mock-llm.yaml"], &artifacts(), &[]);
    let probe = wait_for_probe(&fleet);

    let d = probe.get("/describe");
    assert_eq!(d["provider"], json!("mock-swarm-1"), "config should name the model: {d}");

    // --- three branches, one question, three candidates ----------------------
    // This is a generation of a swarm, simulated. The seed already exists in the
    // contract for reproducibility; here it is what makes branches differ.
    let one = probe.ask("add a cache", 1);
    let two = probe.ask("add a cache", 2);
    let three = probe.ask("add a cache", 3);
    assert_eq!(one["text"], json!("CANDIDATE ONE: memoise the lookup"), "{one}");
    assert_eq!(two["text"], json!("CANDIDATE TWO: put an LRU in front"), "{two}");
    assert_eq!(three["text"], json!("CANDIDATE THREE: cache at the edge"), "{three}");
    assert_ne!(one["text"], two["text"], "branches must be able to differ");

    // A seed with no rule of its own falls through, so a swarm of nineteen does
    // not need nineteen rules written for it.
    let far = probe.ask("add a cache", 99);
    assert_eq!(far["text"], json!("CANDIDATE FALLBACK: do nothing clever"), "{far}");

    // --- the property that makes assertions possible at all ------------------
    for _ in 0..3 {
        let again = probe.ask("add a cache", 2);
        assert_eq!(
            again["text"], two["text"],
            "the same branch asked twice must answer the same, or nothing downstream \
             of this — selection, ranking, the winner — can be asserted at all"
        );
    }

    // --- failure modes, which a real provider will not produce on demand -----
    // These are the interesting paths through a loop, and they are exactly the
    // ones that are expensive or impossible to trigger deliberately upstream.
    let denied = probe.ask("rate limit me", 0);
    assert_eq!(denied["error"], json!("provider-denied"), "a scripted 429 should surface: {denied}");
    assert!(
        denied["detail"].as_str().unwrap_or_default().contains("429"),
        "the reason should survive: {denied}"
    );

    let empty = probe.ask("say nothing", 0);
    assert_eq!(
        empty["error"],
        json!("no-content"),
        "an empty completion is its own case, distinct from a refusal: {empty}"
    );

    // --- a prompt the script never anticipated -------------------------------
    // It answers rather than erroring, because the script has a `*` rule. The
    // point is that this is the SCRIPT's decision: a script without the catch-all
    // fails loudly instead, so a test whose prompt drifted cannot keep passing
    // while silently testing nothing.
    let unknown = probe.ask("something else entirely", 0);
    assert_eq!(unknown["text"], json!("I have no rule for that."), "{unknown}");

    println!("    three branches, three candidates, repeatable — and nothing spent");
}
