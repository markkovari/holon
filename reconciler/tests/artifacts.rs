//! `artifact:cache` on a real fleet: derived work computed once, handed over.
//!
//! The claim mechanism is the entire reason the component exists, and the only
//! thing that can check it is concurrency. A cache with a plain `get`/`put` looks
//! correct in every unit test and is useless in the generation that matters:
//! twenty branches start together, twenty miss the same key at the same instant,
//! and twenty compute the same expensive thing. It starts helping in generation
//! two — after the work has been done twenty times.
//!
//! So the headline assertion here is a number: N concurrent lookups of one key
//! produce exactly ONE claim.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("gate", "artifact_probe.wasm"),
        ("cache", "artifact_cache.wasm"),
        ("blobs", "blob_store.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

#[derive(Clone)]
struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn call(&self, method: reqwest::Method, path: &str, body: Vec<u8>) -> Value {
        let r = match self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "artifacts.acme.test")
            .body(body)
            .send()
        {
            Ok(r) => r,
            // Reported, not panicked on: the readiness loop polls before anything
            // is listening, and a panic there hides "not up yet" behind "broken".
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }

    fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path, Vec::new())
    }

    fn post(&self, path: &str, body: &str) -> Value {
        self.call(reqwest::Method::POST, path, body.as_bytes().to_vec())
    }

    fn lookup(&self, producer: &str, version: &str, inputs: &str, params: &str) -> Value {
        self.get(&format!(
            "/lookup?producer={producer}&version={version}&inputs={inputs}&params={params}"
        ))
    }
}

/// Readiness has to cross the whole chain, not just answer.
///
/// The root route touches no capability, so it answers before the
/// `cache → blobs` link is usable; polling it proves the wrong thing and the
/// next request loses the race under load.
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut last = Value::Null;
    while std::time::Instant::now() < deadline {
        let r = probe.get("/get?id=0000000000000000000000000000000000000000");
        if r["found"] == json!(false) {
            return probe;
        }
        last = r;
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!(
        "the cache app never became able to reach its store — last answer {last}\n\
         --- node ---\n{}\n--- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
}

#[test]
fn twenty_branches_missing_at_once_produce_exactly_one_producer() {
    let fleet =
        Fleet::start_with_secrets("artifacts", &["fixtures/artifact-cache.yaml"], &artifacts(), &[]);
    let probe = wait_for_probe(&fleet);

    // The id is a pure function of the key, and knowable without touching the
    // store — which is how an artifact gets NAMED between agents before anyone
    // knows whether it exists.
    let id = probe.get("/id?producer=chunker&version=1&inputs=tree-abc&params=size%3D800");
    let id = id["id"].as_str().unwrap_or_default().to_string();
    assert_eq!(id.len(), 40, "no id derived: {id:?}");

    // --- the headline: a swarm all missing at once --------------------------
    // Twelve concurrent lookups of one key, the way a generation of branches
    // would arrive. Exactly one may be told to compute.
    let n = 12;
    let results: Vec<Value> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let p = probe.clone();
                s.spawn(move || p.lookup("chunker", "1", "tree-abc", "size%3D800"))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let claimed: Vec<&Value> = results.iter().filter(|r| r["state"] == json!("claimed")).collect();
    let pending = results.iter().filter(|r| r["state"] == json!("pending")).count();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one branch may be told to compute — {} were, {pending} were told to wait. \
         More than one is the thundering herd this component exists to stop; zero means \
         nobody would ever produce it. Answers: {results:?}",
        claimed.len()
    );
    assert_eq!(pending, n - 1, "everyone who did not win should be waiting: {results:?}");

    // The waiters are told how long, not a fixed guess — a caller polling faster
    // than the work can finish is just load.
    let waiter = results.iter().find(|r| r["state"] == json!("pending")).unwrap();
    assert!(
        waiter["retry_ms"].as_u64().unwrap_or(0) > 0,
        "a waiter should be told when to come back: {waiter}"
    );

    // --- the producer produces, and everyone else gets it -------------------
    let token = claimed[0]["claim"].as_str().unwrap().to_string();
    let stored = probe.post(&format!("/put?claim={token}"), "chunk-index-v1");
    assert_eq!(stored["stored"], json!(id), "the artifact did not store: {stored}");

    let r = probe.lookup("chunker", "1", "tree-abc", "size%3D800");
    assert_eq!(r["state"], json!("hit"), "after a put, everyone should hit: {r}");
    assert_eq!(r["content"], json!("chunk-index-v1"), "wrong bytes came back: {r}");

    // And by id alone, which is how it is handed over: a forty-character string
    // travels between agents, not the bytes.
    let r = probe.get(&format!("/get?id={id}"));
    assert_eq!(r["content"], json!("chunk-index-v1"), "fetch by id failed: {r}");

    // --- a new producer version is NOT the old artifact ----------------------
    // Serving v1's output to a v2 request is worse than a miss: it is a cache
    // that silently returns wrong answers.
    let r = probe.lookup("chunker", "2", "tree-abc", "size%3D800");
    assert_eq!(
        r["state"],
        json!("claimed"),
        "a changed producer version must not be served the old artifact: {r}"
    );
    let v2_token = r["claim"].as_str().unwrap().to_string();

    // --- abandoning hands the work back rather than making everyone wait -----
    let done = probe.post(&format!("/abandon?claim={v2_token}"), "");
    assert_eq!(done["abandoned"], json!(true), "abandon failed: {done}");
    let r = probe.lookup("chunker", "2", "tree-abc", "size%3D800");
    assert_eq!(
        r["state"],
        json!("claimed"),
        "after an abandon the next caller should be told to compute, not to wait — \
         otherwise a producer that gave up blocks everyone for the whole lease: {r}"
    );

    // --- a claim is a lease, so a dead producer cannot wedge the key ---------
    // That claim is now held and never satisfied — a branch that died. The
    // fixture sets the lease to 3s, so waiting it out is cheap.
    let r = probe.lookup("chunker", "2", "tree-abc", "size%3D800");
    assert_eq!(r["state"], json!("pending"), "the claim should be held right now: {r}");
    std::thread::sleep(Duration::from_secs(4));
    let r = probe.lookup("chunker", "2", "tree-abc", "size%3D800");
    assert_eq!(
        r["state"],
        json!("claimed"),
        "an expired claim must be takeable — a producer that dies holding a LOCK \
         wedges the key forever, and branches are expected to die: {r}"
    );
    let fresh = r["claim"].as_str().unwrap().to_string();

    // --- and the producer it replaced may not write over it ------------------
    let late = probe.post(&format!("/put?claim={v2_token}"), "stale-result");
    assert_eq!(
        late["error"],
        json!("not-your-claim"),
        "a superseded producer must not overwrite whoever replaced it: {late}"
    );

    // The rightful holder still can.
    let ok = probe.post(&format!("/put?claim={fresh}"), "chunk-index-v2");
    assert_eq!(ok["stored"].as_str().map(str::len), Some(40), "the fresh claim should store: {ok}");
    let r = probe.lookup("chunker", "2", "tree-abc", "size%3D800");
    assert_eq!(r["content"], json!("chunk-index-v2"));

    // v1 is untouched by any of it.
    let r = probe.lookup("chunker", "1", "tree-abc", "size%3D800");
    assert_eq!(r["content"], json!("chunk-index-v1"), "v2's work overwrote v1's: {r}");

    println!("    12 branches, 1 producer, 11 told to wait — and an expired claim recovered");
}
