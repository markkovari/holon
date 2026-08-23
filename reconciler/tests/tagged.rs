//! A lesson reaches a goal that shares an INTERFACE but shares no wording.
//!
//! This is the claim of ADR-0090, and it is arranged so that nothing else can
//! produce the result. Two goals are written to be textually as unlike each other
//! as two software goals can be — a veterinary clinic and a payroll exporter —
//! while touching the same capability, `csv:codec/codec`. The lesson is a fact
//! about that capability and about nothing else:
//!
//!     Dialect.delimiter is a String, not a char.
//!
//! Then the second goal reads twice:
//!
//!   * **with tags** — it must find the lesson.
//!   * **text only** — it must NOT. This arm is the whole test. If similarity
//!     already connects a clinic to a payroll exporter, tags buy nothing and
//!     ADR-0090 is wrong, and this test is how that would be discovered rather
//!     than assumed.
//!
//! No AI calls: a real SurrealDB, the real components, and a mock provider whose
//! embeddings are deterministic. Skipped, loudly, when Docker cannot start.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::Value;

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

/// The two goals, and the point of the test in one place.
///
/// If these two ever start looking alike, the control arm will begin to pass and
/// the test will stop meaning anything — so they are stated here together where a
/// reader can see how little they share.
const CLINIC_GOAL: &str = "export the day's veterinary visits for a clinic, one row per pet";
const PAYROLL_GOAL: &str = "produce a monthly payroll remittance file for the finance team";
const SHARED_TAG: &str = "csv:codec/codec@0.1.0";
const LESSON: &str =
    "csv:codec's Dialect.delimiter is a String, not a char: pass \",\".to_string() \
                      or the call will not compile";

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    [
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
    ]
    .iter()
    .map(|(id, file)| {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        format!("{id}={}", p.display())
    })
    .collect()
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let yaml = std::fs::read_to_string(repo_root().join("fixtures/knowledge-memory.yaml"))
        .unwrap()
        .replace("SURREAL_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-tagged-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

struct Pool {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Pool {
    fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path, String::new())
    }
    fn post(&self, path: &str, body: &str) -> Value {
        self.call(reqwest::Method::POST, path, body.to_string())
    }
    fn call(&self, method: reqwest::Method, path: &str, body: String) -> Value {
        let r = self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "memory.acme.test")
            .body(body)
            .send();
        let Ok(r) = r else { return Value::Null };
        let text = r.text().unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn texts(r: &Value) -> Vec<String> {
    r["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|h| h["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_lesson_crosses_two_goals_that_share_an_interface_and_nothing_else() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real database \
             and Docker to run it in. Nothing about tagged retrieval was verified by \
             this run."
        );
        return;
    };

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(db.port);
    let fleet = Fleet::start_with_secrets(
        "tagged",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let pool = Pool {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap(),
    };
    // The readiness check is a real recall, not a ping: the ingress answers
    // "no replica ... is currently placed" as plain text long before the pool can
    // serve, and that parses into a perfectly good `Value::String`. Requiring the
    // `hits` array is what distinguishes "up" from "answering with an excuse".
    fleet.until("the pool answering a recall", Duration::from_secs(180), || {
        let r = pool.get("/recall?goal=anything&k=1");
        if r.get("hits").is_some() {
            Ok(())
        } else {
            Err(format!("not serving yet: {r}"))
        }
    });

    // --- the clinic learns something about csv:codec ---------------------------
    let wrote = pool.post(
        &format!(
            "/observe?ns=errors&goal={}&env=clinic&attempt=1&tags={}",
            enc(CLINIC_GOAL),
            enc(SHARED_TAG)
        ),
        LESSON,
    );
    assert!(
        wrote["handle"].as_str().unwrap_or_default().starts_with("errors:"),
        "the tagged lesson did not land: {wrote}"
    );

    // --- the payroll exporter asks, by TEXT only -------------------------------
    //
    // The control arm, and the only thing that makes the next assertion mean
    // anything. These two goals have no words in common that matter, so if this
    // finds the lesson then tags are not what connected them.
    let text_only = pool.get(&format!("/recall?goal={}&k=5&min=0.55", enc(PAYROLL_GOAL)));
    let found_by_text = texts(&text_only).iter().any(|t| t.contains("Dialect.delimiter"));

    // --- and again, carrying the interface it imports --------------------------
    // `min=0.55` on every arm, so the dense pass cannot supply the lesson to any
    // of them — the clinic text scores 0.42 against a payroll goal. What is left is
    // the tag, which is the only variable in this experiment.
    let tagged = pool.get(&format!(
        "/recall?goal={}&k=5&min=0.55&tags={}",
        enc(PAYROLL_GOAL),
        enc(SHARED_TAG)
    ));
    let found_by_tag = texts(&tagged).iter().any(|t| t.contains("Dialect.delimiter"));

    assert!(
        found_by_tag,
        "the payroll goal imports {SHARED_TAG} and did not get the lesson written \
         against it: {tagged}"
    );
    assert!(
        !found_by_text,
        "text similarity ALREADY connects a veterinary clinic to a payroll \
         remittance file, so tagging bought nothing here and ADR-0090's premise is \
         wrong — which is worth knowing. Hits were: {:?}",
        texts(&text_only)
    );

    // --- a tag that nothing was written against returns nothing ----------------
    //
    // Otherwise the assertion above could be satisfied by a query that returns the
    // whole pool regardless of what was asked for.
    let other = pool.get(&format!(
        "/recall?goal={}&k=5&min=0.55&tags={}",
        enc(PAYROLL_GOAL),
        enc("nobody:home/iface@0.1.0")
    ));
    assert!(
        !texts(&other).iter().any(|t| t.contains("Dialect.delimiter")),
        "an unrelated tag returned the lesson, so tag matching is not matching: {other}"
    );

    println!(
        "\n  a lesson written against {SHARED_TAG} by a clinic reached a payroll \
         exporter, which shares the interface and none of the wording"
    );
}
