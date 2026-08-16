//! The control plane under test, spawned per test.
//!
//! Extracted from `secrets.rs` when a second suite needed it (ADR-0073). Each
//! test gets its own process on its own port: nextest gives a test its own
//! process but not its own network, and two sharing a port fail only when the
//! suite runs in parallel — which is how it is normally run.

#![allow(dead_code)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// Each test gets its own control plane on its own port. nextest gives a test its
/// own process but not its own network, and two of these sharing a port fail only
/// when the suite is run in parallel — which is how it is normally run.

pub struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub struct Platform {
    _dir: tempfile::TempDir,
    _child: Kill,
    /// Public so a suite can make a request the helpers do not cover — an odd
    /// header, a raw status code — without growing a method per test.
    pub http: reqwest::blocking::Client,
    port: u16,
}

impl Platform {
    pub fn start(port: u16) -> Self {
        let root = repo_root();
        let host = root.join("host/target/release/comp-host");
        assert!(host.exists(), "missing {} — cargo build --release in host/", host.display());
        // Derived when the hand-composed artifact is not there, matching
        // `Fleet::start`: `just compose-platform` is a second recipe a fresh
        // checkout has no reason to have run, and the plug list is already implied
        // by what platform-domain imports.
        let legacy = root.join("components/target/platform_domain.composed.wasm");
        let component = if legacy.is_file() {
            legacy
        } else {
            let catalog = comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&root));
            comp_reconciler::plug::compose_to(
                "platform-domain",
                &catalog,
                &root.join("components/target/composed"),
            )
            .unwrap_or_else(|e| panic!("composing platform-domain: {e} — `just build` first"))
        };

        let dir = tempfile::tempdir().unwrap();
        // A real 32-byte key, per run. The vault seals every value with
        // ChaCha20-Poly1305 under it, so a wrong length is a startup failure rather
        // than a silently weaker secret.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::process::id().hash(&mut h);
            let seed = h.finish().to_le_bytes();
            let raw: Vec<u8> = (0..32).map(|i| seed[i % 8] ^ i as u8).collect();
            base64(&raw)
        };
        let mut cmd = Command::new(host);
        cmd.current_dir(&root)
            .arg("--component")
            .arg(&component)
            .args(["--addr", &format!("127.0.0.1:{port}"), "--kv", "sqlite"])
            .arg("--sqlite-path")
            .arg(dir.path().join("kv.db"))
            .args(["--tenant", "platform", "--app", "control-plane"])
            .args(["--config", "applier-secret=s3cret"])
            .args(["--config", "ingress-suffix=sec.test"])
            .args(["--config", &format!("master-key={key}")]);
        let child = Kill(cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());

        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let me = Self { _dir: dir, _child: child, http, port };
        for _ in 0..60 {
            if me.http.get(me.url("/")).send().is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        panic!("the control plane never came up");
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Register, then log in for a bearer token.
    ///
    /// Two calls because `/api/register` answers with the identity and not a
    /// session — the CLI does the same thing. Registering twice is harmless, so the
    /// result is ignored and the token always comes from the login.
    pub fn user(&self, name: &str) -> String {
        let body = json!({
            "email": format!("{name}@sec.test"),
            "password": format!("correct-horse-{name}"),
        });
        let _ = self.http.post(self.url("/api/register")).json(&body).send();
        let v: Value = self
            .http
            .post(self.url("/api/login"))
            .json(&body)
            .send()
            .unwrap()
            .json()
            .unwrap();
        v["token"].as_str().unwrap_or_else(|| panic!("no token in {v}")).to_string()
    }

    pub fn post(&self, token: &str, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }

    pub fn get(&self, token: &str, path: &str) -> (u16, Value) {
        let r = self.http.get(self.url(path)).bearer_auth(token).send().unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }
}

pub fn base64(raw: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in raw.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}


impl Platform {
    /// A call on the internal API, which takes the applier secret rather than a
    /// session — the reconciler's door, not a user's.
    pub fn post_internal(&self, path: &str, body: serde_json::Value) -> (u16, Value) {
        let r = self
            .http
            .post(self.url(path))
            .header("x-platform-secret", "s3cret")
            .json(&body)
            .send()
            .unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }
}

// ---------------------------------------------------------------------------
// A real SurrealDB, for the suites that need one.
//
// Lived in `graph.rs` until `memory.rs` needed the same database (ADR-0084). Two
// copies of a PINNED image is two places to forget to pin, so it moved here
// rather than being duplicated — the same reason the control plane above did.
// ---------------------------------------------------------------------------
/// The database's password, which appears in no manifest. That is the point of
/// ADR-0010 and it is checkable: a fixture holds a `vault://` reference, and this
/// string is nowhere in it.
pub const SURREAL_PASSWORD: &str = "root-not-in-any-manifest";

/// The image, PINNED. The three response shapes this component's tests encode —
/// backtick-quoted ids, a missing namespace, a missing table reading as an error
/// — were captured from this version. `latest` would let a server upgrade turn
/// into a mystery failure in a test that never changed.
pub const SURREAL_IMAGE: &str = "surrealdb/surrealdb:v3.1.3";

/// A SurrealDB container that dies with the test.
///
/// A container rather than a local binary so the version is the same everywhere
/// this runs and nobody has to install a database to run the suite.
pub struct Surreal {
    name: String,
    pub port: u16,
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
    pub fn start() -> Option<Self> {
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


/// A SurrealDB with a `holon` namespace and a reader for what it answered.
///
/// `Surreal` above gives a container; this gives a way to talk to it. Both
/// capgraph database tests want the same four things — define the namespace,
/// post SurrealQL, read the last statement's result, count a table — and each
/// had its own copy until the second one was written.
///
/// The reason it belongs here rather than in one test that the other imports: a
/// test file is not a library, and the one that happened to be written first is
/// not more canonical than the second. `harness` is where this suite already
/// puts what more than one test needs.
pub struct Store {
    db: Surreal,
    http: reqwest::blocking::Client,
}

/// The coordinates PRODUCTION uses — `knowledge-graph`'s default namespace and
/// the database `comp-goalrun` rewrites the memory app to.
///
/// Not arbitrary test coordinates. This harness used `holon`/`holon` and put both
/// halves of ADR-0091's join there, which is precisely why it passed while the
/// real deployment had the capability graph in one database and the lessons in
/// another. A store fixture that agrees with itself but not with production tests
/// the fixture.
const NS: &str = "comp";
const DB: &str = "goalmemory";

impl Store {
    /// A container with the namespace defined, or `None` when Docker cannot
    /// start one. The caller decides whether that is a skip or a failure.
    pub fn start() -> Option<Self> {
        let db = Surreal::start()?;
        let http = reqwest::blocking::Client::builder()
            // Long, because a stress round posts a lot of statements at once.
            .timeout(Duration::from_secs(600))
            .build()
            .unwrap();
        let me = Self { db, http };
        me.raw(&format!("DEFINE NAMESPACE IF NOT EXISTS {NS};"));
        me.raw(&format!("DEFINE DATABASE IF NOT EXISTS {DB};"));
        Some(me)
    }

    /// The port the container is bound to, for a test that needs to reach it by
    /// some route other than `raw`.
    pub fn port(&self) -> u16 {
        self.db.port
    }

    /// Post SurrealQL and return the per-statement array, unchecked.
    pub fn raw(&self, body: &str) -> Value {
        let text = self
            .http
            .post(format!("http://127.0.0.1:{}/sql", self.db.port))
            .basic_auth("root", Some(SURREAL_PASSWORD))
            .header("accept", "application/json")
            .header("surreal-ns", NS)
            .header("surreal-db", DB)
            .body(body.to_string())
            .send()
            .and_then(|r| r.text())
            .unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// The result of the LAST statement, with every statement checked.
    ///
    /// A rejected statement in the middle of a multi-statement body is the
    /// failure mode that would otherwise surface much later as a mysteriously
    /// empty query, pointing at the read rather than the write that failed.
    pub fn last(&self, body: &str) -> Value {
        let answered = self.raw(body);
        let empty = Vec::new();
        let statements = answered.as_array().unwrap_or(&empty);
        let failed: Vec<&Value> = statements.iter().filter(|s| s["status"] != "OK").collect();
        assert!(failed.is_empty(), "{} statement(s) rejected: {:?}", failed.len(), failed);
        statements.last().map(|s| s["result"].clone()).unwrap_or(Value::Null)
    }

    pub fn count(&self, table: &str) -> u64 {
        self.last(&format!("SELECT count() FROM {table} GROUP ALL;"))[0]["count"]
            .as_u64()
            .unwrap_or(0)
    }

    /// `last`, plus how long the server took to answer.
    pub fn timed(&self, body: &str) -> (Duration, Value) {
        let at = std::time::Instant::now();
        let out = self.last(body);
        (at.elapsed(), out)
    }
}
