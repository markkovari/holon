//! `knowledge:memory` composed through a real host and database; skipped without Docker.

use std::time::Duration;

use holon_fleet::fleet::{repo_root, Fleet};
use holon_goal::memory::{run_id, Memory};
use serde_json::Value;

// Shared harness, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

/// Two apps sharing artifacts: one whose provider embeds, one whose provider cannot.
fn specs_for(port: u16) -> Vec<std::path::PathBuf> {
    ["knowledge-memory.yaml", "knowledge-memory-sparse.yaml"]
        .iter()
        .map(|name| {
            let yaml = std::fs::read_to_string(repo_root().join("fixtures").join(name))
                .unwrap()
                .replace("SURREAL_PORT", &port.to_string());
            let out = std::env::temp_dir().join(format!("comp-{name}-{port}"));
            std::fs::write(&out, yaml).unwrap();
            out
        })
        .collect()
}

struct Probe {
    port: u16,
    host: &'static str,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path)
    }

    fn post(&self, path: &str, body: &str) -> Value {
        self.call_with(reqwest::Method::POST, path, body.to_string())
    }

    fn call(&self, method: reqwest::Method, path: &str) -> Value {
        self.call_with(method, path, String::new())
    }

    fn call_with(&self, method: reqwest::Method, path: &str, body: String) -> Value {
        let r = self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port))
            // The ingress routes on this.
            .header("host", self.host)
            .body(body)
            .send();
        // Not panicked on: the readiness loop polls before anything listens.
        let r = match r {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }
}

/// Retries a real read, not a readiness route, so what is retried is what is measured.
fn wait_for(fleet: &Fleet, host: &'static str) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        host,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap(),
    };
    fleet.until(
        &format!("asking {host} about a goal nobody has evaluated"),
        Duration::from_secs(180),
        || {
            let r = probe.get("/already-done?goal=nothing+has+ever+asked+this");
            if r["found"] == Value::Bool(false) {
                Ok(())
            } else {
                Err(r.to_string())
            }
        },
    );
    probe
}

fn hits(r: &Value) -> &Vec<Value> {
    r["hits"].as_array().unwrap_or_else(|| panic!("no hits array in {r}"))
}

fn keys(r: &Value) -> Vec<String> {
    hits(r).iter().map(|h| h["key"].as_str().unwrap_or_default().to_string()).collect()
}

#[test]
fn five_components_one_link_graph_and_a_provider_that_is_really_asked_to_embed() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real \
             database and Docker to run it in. Nothing about the composed \
             deployment was verified by this run."
        );
        return;
    };

    // Loopback is private; set before the fleet starts, since the hosts inherit it.
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let specs = specs_for(db.port);
    let spec_paths: Vec<&str> = specs.iter().map(|p| p.to_str().unwrap()).collect();
    let fleet = Fleet::start_with_secrets(
        "memory",
        &spec_paths,
        &artifacts(),
        // The password reaches `graph` from the vault; it is in neither manifest.
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let dense = wait_for(&fleet, "memory.acme.test");

    // One POST proves four links; a missing link is a trap, not a wrong answer.
    let goal = "make a slug from a title string";
    let r = dense.post(
        &format!("/observe?ns=errors&goal={}&env=env-1&attempt=1", enc(goal)),
        "lowercasing the title is not enough — punctuation has to go too",
    );
    let handle = r["handle"].as_str().unwrap_or_default().to_string();
    assert!(handle.starts_with("errors:"), "the write did not land: {r}");
    // Re-observing must reinforce one row.
    let again = dense.post(
        &format!(
            "/observe?ns=errors&goal={}&env=env-2&attempt=1",
            enc("  MAKE a Slug from a title STRING\n")
        ),
        "lowercasing the title is not enough — punctuation has to go too, again",
    );
    assert_eq!(
        again["handle"].as_str(),
        Some(handle.as_str()),
        "a differently-spelled goal derived a different key: {again}"
    );

    // `dense: true` only if `llm:inference/embed` was called and KNN found the row by it.
    let r = dense.get(&format!("/recall?goal={}&k=3", enc(goal)));
    assert_eq!(keys(&r), vec![handle.clone()], "the lesson was not retrieved: {r}");
    assert_eq!(hits(&r)[0]["dense"], Value::Bool(true), "nothing embedded: {r}");
    assert!(
        hits(&r)[0]["similarity"].as_f64().unwrap_or(0.0) > 0.5,
        "a hit found by its own goal should be close to it: {r}"
    );

    // Asking both shows neither retriever is quietly missing.
    let r = dense.get(&format!("/recall?goal={}&k=3", enc("slug title string")));
    assert_eq!(keys(&r), vec![handle.clone()], "the sparse index found nothing: {r}");

    // The agent verb cannot write the trusted pool and the gated verb can: linked separately.
    let r = dense.post(
        &format!("/observe?ns=patterns&goal={}&env=env-1&attempt=1", enc(goal)),
        "this should never be stored",
    );
    assert_eq!(r["error"], Value::String("refused".into()), "observe wrote patterns: {r}");

    let r = dense.post(
        &format!("/promote?goal={}&score=0&env=env-1&attempt=1", enc(goal)),
        "a lesson downstream of a failure",
    );
    assert_eq!(r["error"], Value::String("refused".into()), "a failing gate promoted: {r}");

    let r = dense.post(
        &format!("/promote?goal={}&score=1000&env=env-1&attempt=2", enc(goal)),
        "split on syntax, not token count",
    );
    let promoted = r["handle"].as_str().unwrap_or_default().to_string();
    assert!(promoted.starts_with("patterns:"), "the gate could not promote: {r}");

    // Asking for one pool returns only that one.
    let r = dense.get(&format!("/recall?goal={}&k=5", enc(goal)));
    assert_eq!(keys(&r).len(), 2, "both pools should answer: {r}");
    let r = dense.get(&format!("/recall?goal={}&k=5&pools=patterns", enc(goal)));
    assert_eq!(keys(&r), vec![promoted.clone()], "a pool filter should narrow: {r}");
    let r = dense.get(&format!("/recall?goal={}&k=0", enc(goal)));
    assert!(hits(&r).is_empty(), "k=0 is the cold control arm: {r}");

    let r = dense.post(&format!("/attribute?keys={promoted}&run=run-1&ok=true"), "");
    assert_eq!(r["ok"], Value::Bool(true), "attribute failed: {r}");
    let r = dense.post(&format!("/attribute?keys={handle}&run=run-2&ok=false"), "");
    assert_eq!(r["ok"], Value::Bool(true), "attribute failed: {r}");
    let r = dense.get(&format!("/recall?goal={}&k=5", enc(goal)));
    assert_eq!(
        keys(&r).first().map(String::as_str),
        Some(promoted.as_str()),
        "the lesson runs passed with should now rank first: {r}"
    );
    // An unknown handle is a no-op, not a resurrection or a failure.
    let r = dense.post("/attribute?keys=errors:deleted-by-a-human&run=run-3&ok=true", "");
    assert_eq!(r["ok"], Value::Bool(true), "an unknown handle should be a no-op: {r}");

    let r = dense.post(
        &format!("/evaluated?goal={}&run=run-9&score=1000&passed=true&artifact=pr%2F41", enc(goal)),
        "",
    );
    assert_eq!(r["ok"], Value::Bool(true), "evaluated failed: {r}");
    let r = dense.get(&format!("/already-done?goal={}", enc(goal)));
    assert_eq!(r["found"], Value::Bool(true), "the same goal is done work: {r}");
    assert_eq!(r["artifact"], Value::String("pr/41".into()), "the artifact did not survive: {r}");
    assert_eq!(r["evaluations"], Value::from(1), "one evaluation was recorded: {r}");
    assert!(
        r["similarity"].as_f64().unwrap_or(0.0) > 0.999,
        "an identical goal is similarity ~1.0: {r}"
    );
    // The floor matters: KNN always returns its nearest row.
    let r =
        dense.get(&format!("/already-done?goal={}", enc("parse a csv file into typed records")));
    assert_eq!(r["found"], Value::Bool(false), "an unrelated goal must not be skipped: {r}");

    // `mock-embeddings=false` refuses `embed` like `anthropic-provider`: retrieval must degrade.
    let sparse = wait_for(&fleet, "memorysparse.acme.test");
    let r = sparse.post(
        &format!("/observe?ns=errors&goal={}&env=env-1&attempt=1", enc(goal)),
        "lowercasing the title is not enough — punctuation has to go too",
    );
    assert!(
        r["handle"].as_str().is_some(),
        "a write must not fail because the deployment cannot embed: {r}"
    );
    let r = sparse.get(&format!("/recall?goal={}&k=3", enc(goal)));
    assert_eq!(keys(&r).len(), 1, "sparse-only retrieval still retrieves: {r}");
    assert_eq!(
        hits(&r)[0]["dense"],
        Value::Bool(false),
        "a deployment with no embedding model must say so on every hit: {r}"
    );
    assert_eq!(
        hits(&r)[0]["similarity"],
        Value::from(0.0),
        "there is no cosine to report when nothing embedded: {r}"
    );
    sparse.post(
        &format!("/evaluated?goal={}&run=run-1&score=900&passed=true&artifact=pr%2F7", enc(goal)),
        "",
    );
    let r = sparse.get(&format!("/already-done?goal={}", enc(goal)));
    assert_eq!(r["found"], Value::Bool(true), "the exact-key path should still work: {r}");
    assert_eq!(r["similarity"], Value::from(1.0), "an exact match is reported as 1.0: {r}");
    let r = sparse.get(&format!("/already-done?goal={}", enc("make a slug from the title string")));
    assert_eq!(
        r["found"],
        Value::Bool(false),
        "without embeddings a paraphrase is NOT recognised — the cost of no provider, \
         and it must be a miss rather than a wrong hit: {r}"
    );

    // The exact client `comp-goalrun` calls, not raw HTTP.
    let client = Memory {
        url: format!("http://127.0.0.1:{}", fleet.ingress_port),
        host: "memory.acme.test".to_string(),
        timeout: Duration::from_secs(30),
    };
    let fresh = "add a retry to the webhook relay";
    assert!(
        client.already_done(fresh, 0.9).expect("the pool answered").is_none(),
        "a goal nobody has evaluated is not done work"
    );

    let seed = 4242;
    for (branch, score, accepted) in [
        ("risk-first", 300u64, false),
        ("mvp-first", 0, false),
        ("user-first", 1000, true),
        ("cold", 250, false),
    ] {
        client
            .evaluated(fresh, &run_id(seed, 0, branch), score, accepted, "")
            .expect("a verdict was refused");
    }
    let prior =
        client.already_done(fresh, 0.9).expect("the pool answered").expect("now it is done");
    assert_eq!(prior.evaluations, 4, "every branch's verdict is on record: {}", prior.summary());
    assert_eq!(prior.score, 1000, "the winner's score, not the last branch's");
    assert_eq!(prior.run, run_id(seed, 0, "user-first"));
    assert!(prior.artifact.is_empty(), "nothing has been opened yet");

    // Keyed by (goal, run), so the PR attaches without a fifth verdict.
    client
        .evaluated(fresh, &run_id(seed, 0, "user-first"), 1000, true, "https://github.test/pr/7")
        .expect("the re-report was refused");
    let prior = client.already_done(fresh, 0.9).expect("the pool answered").expect("still done");
    assert_eq!(prior.evaluations, 4, "a re-reported run is one verdict, not two");
    assert_eq!(prior.artifact, "https://github.test/pr/7", "the pull request is now the answer");
    assert!(prior.summary().contains("pr/7"), "and a human can read it: {}", prior.summary());

    // The runner's path (client, routes, component, store), where the shape can be lost.
    let parent = "serve invoices over http";
    let (back, front) = ("store invoices in postgres", "render the invoice list");
    assert!(
        client.parts_of(parent).expect("the pool answered").is_empty(),
        "a goal nobody decomposed has no parts, and that is an answer not an error"
    );
    client.decomposed_into(parent, back, 0, "the data half").expect("refused");
    client.decomposed_into(parent, front, 1, "the view half").expect("refused");

    let parts = client.parts_of(parent).expect("the pool answered");
    assert_eq!(parts.len(), 2, "both parts came back");
    assert_eq!(parts[0].goal, back, "in ordinal order");
    assert_eq!(parts[0].why, "the data half", "the reason rides on the edge");
    assert!(!parts[0].done, "nothing has been evaluated, so nothing is done");

    client.evaluated(back, &run_id(seed, 0, "backend"), 1000, true, "").expect("refused");
    let parts = client.parts_of(parent).expect("the pool answered");
    let done_for = |g: &str| parts.iter().find(|p| p.goal == g).map(|p| p.done);
    assert_eq!(done_for(back), Some(true), "a passing verdict finishes that part");
    assert_eq!(
        done_for(front),
        Some(false),
        "and ONLY that part — a parent is resumable or it is not"
    );

    // A shared sub-goal has two parents and remains one node.
    let other = "produce a monthly billing report";
    client.decomposed_into(other, back, 0, "needs the same store").expect("refused");
    let owners = client.parents_of(back).expect("the pool answered");
    assert_eq!(owners.len(), 2, "a shared sub-goal has two parents: {:?}", owners.len());
    assert!(owners.iter().any(|p| p.goal == parent) && owners.iter().any(|p| p.goal == other));

    // Idempotent: re-running a decomposed goal is normal.
    client.decomposed_into(parent, back, 0, "the data half").expect("refused");
    assert_eq!(
        client.parts_of(parent).expect("the pool answered").len(),
        2,
        "a second decomposition reinforces the edges rather than duplicating them"
    );

    // Refused where it is cheap, not left to a depth bound.
    assert!(
        client.decomposed_into(parent, "  Serve Invoices Over HTTP ", 0, "same thing").is_err(),
        "a goal that normalises to itself must be refused"
    );

    // Four failures are a reason to think, not finished work.
    let hard = "make the flaky integration suite deterministic";
    for branch in ["risk-first", "mvp-first"] {
        client.evaluated(hard, &run_id(seed, 0, branch), 0, false, "").expect("refused");
    }
    assert!(
        client.already_done(hard, 0.9).expect("the pool answered").is_none(),
        "a goal that only ever failed is not done work"
    );

    // An error, never `Ok(None)`: the runner turns an error into "do the work".
    let missing = Memory {
        url: format!("http://127.0.0.1:{}", fleet.ingress_port),
        host: "nosuchapp.acme.test".to_string(),
        timeout: Duration::from_secs(5),
    };
    assert!(
        missing.already_done(fresh, 0.9).is_err(),
        "an unreachable pool must report a failure rather than answer 'not done'"
    );

    // It spares what was just written, and refuses `days` of 0 (ADR-0081).
    assert!(
        client.decay(0, 2).is_err(),
        "a max age of zero would forget everything nobody has read yet"
    );
    let before = client.already_done(fresh, 0.9).expect("the pool answered");
    let forgotten = client.decay(30, 2).expect("the sweep ran");
    assert_eq!(forgotten, 0, "nothing written in this test is a month old");
    assert!(
        client.already_done(fresh, 0.9).expect("the pool answered").is_some(),
        "a sweep must not take work that was recorded seconds ago: {:?}",
        before.map(|p| p.summary())
    );

    // The two apps are separate stores.
    let r = sparse.get(&format!("/recall?goal={}&k=5&pools=patterns", enc(goal)));
    assert!(
        hits(&r).is_empty(),
        "one app read another app's pool — a store is named after its app (ADR-0023): {r}"
    );
}

fn enc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => other.to_string().bytes().map(|b| format!("%{b:02X}")).collect::<String>(),
        })
        .collect()
}
