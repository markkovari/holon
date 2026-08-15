//! A statement larger than 4096 bytes, through the graph's escape hatch.
//!
//! Both halves of this test exist because of one real run.
//!
//! `wasi:io`'s `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS
//! above that rather than returning an error, so a component that writes a big
//! payload in one call simply dies. `knowledge-graph` wrote every statement that
//! way. Nothing noticed for as long as every statement was small — and then a
//! contract file grew from 3645 bytes to 4573, and `comp-goalrun` died with
//! `every replica of "goalcontract.acme.test" failed; n1 refused`: no size, no
//! component, no mention of a write, three links from the cause.
//!
//! It survived that long because it can only be reached through `query`, the raw
//! SurrealQL hatch, and `graph-probe` exposed every typed verb EXCEPT that one.
//! `contract-registry` does all of its reads and writes through it. So the fix
//! came with a route, and this is the test that route exists for.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::Value;

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    [("gate", "graph_probe.wasm"), ("graph", "knowledge_graph.wasm")]
        .iter()
        .map(|(id, file)| {
            let p = dir.join(file);
            assert!(p.exists(), "missing {} — run `just build`", p.display());
            format!("{id}={}", p.display())
        })
        .collect()
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let yaml = std::fs::read_to_string(repo_root().join("fixtures/knowledge-graph.yaml"))
        .unwrap()
        .replace("SURREAL_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-bigstatement-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

fn post(port: u16, path: &str, body: &str) -> Value {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let r = match http
        .post(format!("http://127.0.0.1:{port}{path}"))
        .header("host", "graph.acme.test")
        .body(body.to_string())
        .send()
    {
        Ok(r) => r,
        Err(e) => return Value::String(format!("transport: {e}")),
    };
    let (status, text) = (r.status(), r.text().unwrap_or_default());
    serde_json::from_str(&text).unwrap_or(Value::String(format!("HTTP {status}: {text}")))
}

#[test]
fn a_statement_over_four_kilobytes_round_trips() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real database and \
             Docker to run it in. Nothing about large statements was verified by this run."
        );
        return;
    };

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let fleet = Fleet::start_with_secrets(
        "bigstatement",
        &[spec_for(db.port).to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let port = fleet.ingress_port;

    // The readiness check IS a `query`, so the hatch is what gets waited on rather
    // than something adjacent to it.
    fleet.until("a small query through the escape hatch", Duration::from_secs(180), || {
        match post(port, "/query", "SELECT * FROM nothing_here;") {
            Value::String(s) => Err(s),
            _ => Ok(()),
        }
    });

    // Comfortably over the limit, and a size a contract really reaches: the file
    // that broke the run was 4573 bytes.
    let big = "x".repeat(12_000);
    let statement = format!("UPSERT big:1 SET body = '{big}';");
    assert!(statement.len() > 4096, "the point of the test is a statement over the limit");
    let wrote = post(port, "/query", &statement);
    assert!(
        !matches!(&wrote, Value::String(s) if s.starts_with("transport:")),
        "the component died writing a {} byte statement — this is the 4096-byte trap: {wrote}",
        statement.len()
    );

    // Written is not enough: the whole body has to have arrived, because a partial
    // write would also "succeed".
    let read = post(port, "/query", "SELECT body FROM big:1;");
    let text = read.to_string();
    assert!(
        text.contains(&"x".repeat(200)),
        "the row came back without its body — the statement was truncated: {}",
        &text[..text.len().min(300)]
    );
    let stored = text.matches('x').count();
    assert_eq!(stored, 12_000, "stored {stored} bytes of a 12000 byte value");

    println!("\n  a {} byte statement wrote and read back whole", statement.len());
}
