//! `knowledge:graph/store` against a real SurrealDB, through a real host.
//!
//! The component's own tests pin the SurrealQL and JSON to live-captured shapes. What they cannot
//! cover: whether `wasi:http/outgoing-handler` carries the statement out of the sandbox, whether the
//! host's egress allow-list lets it through, whether the composer links a non-`wasi` component
//! interface, and whether the password arrives from the vault rather than a manifest (the half that has
//! been wrong before: `comp:secrets/reader` shipped unlinked).
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::time::Duration;

use holon_fleet::fleet::{repo_root, Fleet};

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};
use serde_json::Value;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [("gate", "graph_probe.wasm"), ("graph", "knowledge_graph.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

/// The fixture with the database's real port in it, written outside the repo.
fn spec_for(port: u16) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/knowledge-graph.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("SURREAL_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-knowledge-graph-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
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
            .header("host", "graph.acme.test")
            .body(body)
            .send();
        // Reported, not panicked on: the readiness loop polls before anything listens.
        let r = match r {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        // An unparseable answer is nearly always the ingress or host talking, and a bare `null` hides which.
        serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }
}

/// The first real read, retried until it works. NOT a separate readiness probe: polling the root route
/// touches no capability and answered before the link, egress and SurrealDB were usable. The thing
/// retried IS the thing measured (`Fleet::until`).
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap(),
    };
    fleet.until("reading a node that was never written", Duration::from_secs(120), || {
        let r = probe.get("/get?kind=readiness&id=nothing-here");
        // `found: false` can only be known by asking SurrealDB, so it proves the whole chain and warms
        // namespace creation.
        if r["found"] == Value::Bool(false) {
            Ok(())
        } else {
            Err(r.to_string())
        }
    });
    probe
}

#[test]
fn a_component_writes_a_graph_to_a_real_database_and_walks_it() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real \
             database and Docker to run it in"
        );
        return;
    };

    // Loopback is a private address and the host refuses those unless told otherwise. Set before the
    // fleet starts, since the hosts inherit it.
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(db.port);
    let fleet = Fleet::start_with_secrets(
        "graph",
        &[spec.to_str().unwrap()],
        &artifacts(),
        // The password reaches the component from the vault; it is in no manifest (ADR-0010), and the
        // fixture holds a `vault://` reference.
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let probe = wait_for_probe(&fleet);

    // --- write ---
    // An id with a slash in it: ids are file paths in the intended use, and `/` is the character most
    // likely to be mangled by quoting.
    let r = probe.post("/upsert?kind=file&id=src%2Flib.rs", r#"{"lines":12}"#);
    assert_eq!(r["ok"], Value::Bool(true), "the first write failed: {r}");

    // Round-tripping proves the whole path: statement out through egress, answer back, quoting undone.
    let r = probe.get("/get?kind=file&id=src%2Flib.rs");
    assert_eq!(r["id"], Value::String("src/lib.rs".into()), "the id did not survive: {r}");
    assert_eq!(r["properties"]["lines"], Value::from(12), "the properties did not survive: {r}");

    // A fresh database has no `symbol` table; asking anyway is what an agent does and must read as absence.
    let r = probe.get("/get?kind=symbol&id=nothing-here");
    assert_eq!(r["found"], Value::Bool(false), "a node nobody wrote is absent, not an error: {r}");

    let r = probe.post(
        "/relate?kind=file&id=src%2Flib.rs&edge=defines&to-kind=symbol&to-id=record_id",
        r#"{"at":102}"#,
    );
    assert_eq!(r["ok"], Value::Bool(true), "relate failed: {r}");

    let r = probe.get("/neighbours?kind=file&id=src%2Flib.rs&edge=defines&dir=out");
    let out = r["nodes"].as_array().cloned().unwrap_or_default();
    assert_eq!(out.len(), 1, "one hop out should find the symbol: {r}");
    assert_eq!(out[0]["kind"], Value::String("symbol".into()));
    assert_eq!(out[0]["id"], Value::String("record_id".into()));

    // And backwards, which makes a graph worth having: from a symbol to every file defining it.
    let r = probe.get("/neighbours?kind=symbol&id=record_id&edge=defines&dir=in");
    let back = r["nodes"].as_array().cloned().unwrap_or_default();
    assert_eq!(back.len(), 1, "one hop back should find the file: {r}");
    assert_eq!(back[0]["id"], Value::String("src/lib.rs".into()));

    // An edge nobody has drawn is empty, not broken.
    let r = probe.get("/neighbours?kind=file&id=src%2Flib.rs&edge=imports&dir=out");
    assert_eq!(r["nodes"], Value::Array(vec![]), "an unused edge should be empty: {r}");

    println!("    wrote a graph to SurrealDB and walked it in both directions");
}
