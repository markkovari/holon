//! `knowledge:graph/store` against a real SurrealDB, through a real host.
//!
//! The component's own tests cover the SurrealQL it builds and the JSON it reads
//! back — both pinned to shapes captured from a live server. What they cannot
//! cover is everything between the two: whether `wasi:http/outgoing-handler`
//! carries the statement out of the sandbox at all, whether the host's egress
//! allow-list lets it through, whether the composer links a component interface
//! that is not `wasi:*`, and whether the password arrives from the vault rather
//! than from a manifest.
//!
//! That is the half that has been wrong before in this repo — `comp:secrets/reader`
//! shipped unlinked and its ADR's every claim was untested — so it gets a test
//! that starts a database and asks it questions.
//!
//! Skipped, loudly, when Docker cannot start the database. A skipped test that
//! says so is honest; one that passes because it did nothing is not.

use std::process::{Command, Stdio};
use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::Value;

/// The image, PINNED. The three response shapes this component's tests encode —
/// backtick-quoted ids, a missing namespace, a missing table reading as an error
/// — were captured from this version. `latest` would let a server upgrade turn
/// into a mystery failure in a test that never changed.
const SURREAL_IMAGE: &str = "surrealdb/surrealdb:v3.1.3";

/// A SurrealDB container that dies with the test.
///
/// A container rather than a local binary so the version is the same everywhere
/// this runs and nobody has to install a database to run the suite.
struct Surreal {
    name: String,
    port: u16,
}

impl Drop for Surreal {
    fn drop(&mut self) {
        // `--rm` handles the ordinary exit; this covers a killed test run, which
        // is exactly when a leaked container would otherwise sit holding a port.
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Surreal {
    fn start() -> Option<Self> {
        // `free_port` is the harness's, so the database is bound the same way the
        // rest of the fleet is and two concurrent runs do not collide.
        let port = comp_reconciler::fleet::free_port();
        let name = format!("comp-test-surreal-{port}");
        let status = Command::new("docker")
            .args(["run", "--rm", "-d", "--name", &name])
            // Bound to loopback explicitly: the container must not be reachable
            // from the network just because a test is running.
            .args(["-p", &format!("127.0.0.1:{port}:8000")])
            .arg(SURREAL_IMAGE)
            .args(["start", "--no-banner"])
            .args(["--user", "root", "--pass", SURREAL_PASSWORD])
            // Inside the container; the port mapping above is what the host sees.
            .args(["--bind", "0.0.0.0:8000"])
            .arg("memory")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let me = Self { name, port };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        // A container has an image pull and a runtime start in front of it, so
        // this waits longer than a local process would need.
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while std::time::Instant::now() < deadline {
            if client.get(format!("http://127.0.0.1:{port}/health")).send().is_ok() {
                return Some(me);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        // `me` drops here, so the container goes with it.
        panic!("the {SURREAL_IMAGE} container never became healthy on {port}");
    }
}

const SURREAL_PASSWORD: &str = "root-not-in-any-manifest";

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [("gate", "graph_probe.wasm"), ("graph", "knowledge_graph.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
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
        // Reported, not panicked on: the readiness loop polls before anything is
        // listening, and a panic there hides "not up yet" behind "broken".
        let r = match r {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        // An unparseable answer is nearly always the ingress or the host talking,
        // not the probe — and a bare `null` in the assertion hides which.
        serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }
}

/// The first real read, retried until it works.
///
/// NOT a separate readiness probe. This test used to poll the root route, which
/// touches no capability and answered before the link, the egress and SurrealDB
/// were usable — green alone, red under load. `Fleet::until` exists so that
/// mistake has nowhere to live: the thing retried IS the thing measured.
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build().unwrap(),
    };
    fleet.until("reading a node that was never written", Duration::from_secs(120), || {
        let r = probe.get("/get?kind=readiness&id=nothing-here");
        // `found: false` can only be known by asking SurrealDB, so it proves the
        // whole chain — and it warms the namespace creation, so the first real
        // write is not also the first schema change.
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

    // Loopback is a private address, and the host refuses those unless told
    // otherwise. Set before the fleet starts, since the hosts inherit it.
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(db.port);
    let fleet = Fleet::start_with_secrets(
        "graph",
        &[spec.to_str().unwrap()],
        &artifacts(),
        // The password reaches the component from the vault. It is in no manifest,
        // which is the point of ADR-0010 and is checkable: the fixture holds a
        // `vault://` reference and this string appears nowhere in it.
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let probe = wait_for_probe(&fleet);

    // --- write ---------------------------------------------------------------
    // An id with a slash in it, because ids are file paths in the use this exists
    // for, and a `/` is the character most likely to be mangled by the quoting on
    // the way out or the way back.
    let r = probe.post("/upsert?kind=file&id=src%2Flib.rs", r#"{"lines":12}"#);
    assert_eq!(r["ok"], Value::Bool(true), "the first write failed: {r}");

    // Round-tripping proves the whole path: statement out through egress, answer
    // back, quoting undone.
    let r = probe.get("/get?kind=file&id=src%2Flib.rs");
    assert_eq!(r["id"], Value::String("src/lib.rs".into()), "the id did not survive: {r}");
    assert_eq!(r["properties"]["lines"], Value::from(12), "the properties did not survive: {r}");

    // A fresh database has no `symbol` table. Asking anyway is what an agent does
    // on its first question about a kind, and it must read as absence.
    let r = probe.get("/get?kind=symbol&id=nothing-here");
    assert_eq!(r["found"], Value::Bool(false), "a node nobody wrote is absent, not an error: {r}");

    // --- the hop record:store cannot do --------------------------------------
    let r = probe.post("/relate?kind=file&id=src%2Flib.rs&edge=defines&to-kind=symbol&to-id=record_id", r#"{"at":102}"#);
    assert_eq!(r["ok"], Value::Bool(true), "relate failed: {r}");

    let r = probe.get("/neighbours?kind=file&id=src%2Flib.rs&edge=defines&dir=out");
    let out = r["nodes"].as_array().cloned().unwrap_or_default();
    assert_eq!(out.len(), 1, "one hop out should find the symbol: {r}");
    assert_eq!(out[0]["kind"], Value::String("symbol".into()));
    assert_eq!(out[0]["id"], Value::String("record_id".into()));

    // And backwards, which is the direction that makes a graph worth having: from
    // a symbol to every file that defines it, without an index per question.
    let r = probe.get("/neighbours?kind=symbol&id=record_id&edge=defines&dir=in");
    let back = r["nodes"].as_array().cloned().unwrap_or_default();
    assert_eq!(back.len(), 1, "one hop back should find the file: {r}");
    assert_eq!(back[0]["id"], Value::String("src/lib.rs".into()));

    // An edge nobody has drawn is empty, not broken.
    let r = probe.get("/neighbours?kind=file&id=src%2Flib.rs&edge=imports&dir=out");
    assert_eq!(r["nodes"], Value::Array(vec![]), "an unused edge should be empty: {r}");

    println!("    wrote a graph to SurrealDB and walked it in both directions");
}
