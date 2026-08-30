//! The gate harness, in Rust — what `components/gate-lib.sh` is, without the tools.
//!
//! A gate judges a composed application over real HTTP against a real host. That part
//! is unchanged. What changes is what a machine must have installed to run one.
//!
//! The shell harness needs THIRTEEN external tools — `curl`, `python3`, `grep`, `sed`,
//! `awk`, `mktemp`, `base64`, `date`, `wasm-tools`, `cargo`, `go`, `MailHog`, `docker`
//! — each at a compatible version, on every worker. This needs `comp-host` and a
//! composed `.wasm`, both artifacts this repository builds, so a scheduler ships them
//! rather than provisioning them.
//!
//! ## What is deliberately preserved
//!
//! Failure messages, verbatim. ADR-0088 makes a gate's output the next prompt a repair
//! reads, and the sentences in these gates were written with that in mind — one of them
//! records a round where a gate guessed a cause, reported the guess as a finding, and
//! sent a repair to fix a query that was working. A port that rewords them changes what
//! the loop reads.
//!
//! ## Ports
//!
//! Each gate binds its own, derived from its name, because `cargo test` runs integration
//! binaries in parallel and two gates sharing a port fail only under load — the failure
//! `harness/mod.rs` already documents for the control plane.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// A port unique to this GATE, not to the app it drives.
///
/// The app name alone is not enough and the mistake is worth recording: `cargo test`
/// runs the tests inside one binary on parallel threads, so three triage gates named
/// their port after `triage`, started three hosts on it, and two failed with
/// "comp-host never answered /health" while the third passed. `harness/mod.rs`
/// documents the same failure for the control plane — "two sharing a port fail only
/// when the suite runs in parallel, which is how it is normally run".
///
/// The app hash spreads binaries apart; the counter separates gates inside one.
fn next_port(app: &str) -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let mut h: u32 = 2166136261;
    for b in app.as_bytes() {
        h = (h ^ *b as u32).wrapping_mul(16777619);
    }
    let base = 30000 + (h % 18000) as u16;
    base + NEXT.fetch_add(1, Ordering::SeqCst)
}

/// A running application: one host process, killed when the gate ends or panics.
pub struct Gate {
    child: Child,
    base: String,
    client: reqwest::blocking::Client,
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Compose the crate against what it imports, in this process.
///
/// `gate_compose` in the shell harness shelled to `comp-plug`. `plug::compose_to` is
/// the library that binary is a shim over — ADR-0087 says so in as many words — so a
/// gate calls it directly and `comp-plug` leaves the list of things a worker needs.
///
/// Keyed by content, so a rerun against an unchanged tree reuses the artifact rather
/// than composing again.
pub fn compose(crate_name: &str) -> Option<PathBuf> {
    use comp_reconciler::plug::{compose_to, default_dirs, Catalog};
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED [{crate_name}]: nothing is built — run `just build`");
        return None;
    }
    if catalog.bytes(crate_name).is_none() {
        eprintln!("SKIPPED [{crate_name}]: not built — run `just build`");
        return None;
    }
    match compose_to(crate_name, &catalog, &root.join("components/target")) {
        Ok(path) => Some(path),
        Err(e) => panic!("[{crate_name}] does not compose: {e}"),
    }
}

/// What a gate needs before it can run, or a reason it cannot.
///
/// A loud skip rather than a failure: a checkout that has not built the artifacts has
/// not broken anything, and a gate that fails for a missing file trains people to
/// ignore gate failures.
pub fn artifacts(app: &str, composed: &str) -> Option<(PathBuf, PathBuf)> {
    let root = repo_root();
    let host = root.join("host/target/release/comp-host");
    let wasm = root.join("components/target").join(composed);
    if !host.exists() {
        eprintln!("SKIPPED [{app}]: no comp-host — cargo build --release --manifest-path host/Cargo.toml --bin comp-host");
        return None;
    }
    if !wasm.exists() {
        eprintln!("SKIPPED [{app}]: no {composed} — run `just compose-{app}`");
        return None;
    }
    Some((host, wasm))
}

impl Gate {
    /// Start the app on its own port and wait for it to answer.
    ///
    /// `config` is the `--config key=value` pairs the app needs; the two every gate
    /// sets are added here so no gate has to remember them.
    /// Compose `crate_name` here and serve it as `app`. The shape every gate wants:
    /// nothing has to have run `just compose-…` first.
    pub fn compose_and_start(app: &str, crate_name: &str, config: &[&str]) -> Option<Self> {
        let wasm = compose(crate_name)?;
        let host = repo_root().join("host/target/release/comp-host");
        if !host.exists() {
            eprintln!("SKIPPED [{app}]: no comp-host — cargo build --release --manifest-path host/Cargo.toml --bin comp-host");
            return None;
        }
        Some(Self::serve(app, &host, &wasm, config))
    }

    pub fn start(app: &str, composed: &str, config: &[&str]) -> Option<Self> {
        let (host, wasm) = artifacts(app, composed)?;
        Some(Self::serve(app, &host, &wasm, config))
    }

    fn serve(app: &str, host: &std::path::Path, wasm: &std::path::Path, config: &[&str]) -> Self {
        let port = next_port(app);
        let addr = format!("127.0.0.1:{port}");

        let mut args: Vec<String> = vec![
            "--app".into(), app.into(),
            "--config".into(), format!("default-tenant={app}"),
            "--config".into(), "allow-test-routes=true".into(),
        ];
        for c in config {
            args.push("--config".into());
            args.push((*c).to_string());
        }
        args.extend([
            "--component".into(), wasm.to_string_lossy().into_owned(),
            "--addr".into(), addr.clone(),
        ]);

        let child = Command::new(host)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn comp-host: {e}"));

        let gate = Gate {
            child,
            base: format!("http://{addr}"),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
        };
        for _ in 0..200 {
            if gate.client.get(format!("{}/health", gate.base)).send().is_ok() {
                return gate;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("[{app}] comp-host never answered /health on {addr}");
    }

    /// (status, body). A non-2xx is a VALUE: most of a gate is asserting that a
    /// particular request is refused with a particular code.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<(&str, Vec<u8>)>,
    ) -> (u16, String) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut r = self.client.request(m, format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("authorization", format!("Bearer {t}"));
        }
        if let Some((ct, bytes)) = body {
            r = r.header("content-type", ct).body(bytes);
        }
        let resp = r.send().unwrap_or_else(|e| panic!("{method} {path}: transport error: {e}"));
        (resp.status().as_u16(), resp.text().unwrap_or_default())
    }

    pub fn json(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, String) {
        self.send(method, path, token, body.map(|b| ("application/json", b.to_string().into_bytes())))
    }
    pub fn get(&self, path: &str, token: Option<&str>) -> (u16, String) {
        self.json("GET", path, token, None)
    }
    pub fn post(&self, path: &str, token: Option<&str>, body: Value) -> (u16, String) {
        self.json("POST", path, token, Some(body))
    }
    pub fn patch(&self, path: &str, token: Option<&str>, body: Value) -> (u16, String) {
        self.json("PATCH", path, token, Some(body))
    }
    pub fn delete(&self, path: &str, token: Option<&str>) -> (u16, String) {
        self.json("DELETE", path, token, None)
    }

    /// `stored` in the shell harness: read a document back through the test route, to
    /// assert the SHAPE that was written rather than what a handler chose to return.
    pub fn stored(&self, kind: &str, id: &str) -> String {
        self.get(&format!("/test/{kind}/{id}"), None).1
    }

    /// The fixture. Returns the parsed body, because every gate reaches into it
    /// differently and a typed struct per app would be a second contract to keep.
    pub fn seed(&self) -> Value {
        let (_, raw) = self.post("/test/seed", None, serde_json::json!({}));
        serde_json::from_str(&raw).unwrap_or_else(|_| panic!("the fixture did not come back as JSON: {raw}"))
    }

    /// Bytes and content-type, for the routes that answer with neither JSON nor text.
    pub fn bytes(&self, path: &str, token: Option<&str>) -> (u16, String, Vec<u8>) {
        let mut r = self.client.get(format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("authorization", format!("Bearer {t}"));
        }
        let resp = r.send().unwrap_or_else(|e| panic!("GET {path}: transport error: {e}"));
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (status, ct, resp.bytes().map(|b| b.to_vec()).unwrap_or_default())
    }
}

/// One top-level key out of a JSON body, empty when absent — `field` in the shell
/// harness, and the same emptiness every caller there compares against.
pub fn field(body: &str, key: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get(key).cloned())
        .map(|v| match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// Every gate asserts this: a route behind auth refuses an anonymous caller. It is one
/// line per gate and it catches the part that read the contract's routes and skipped
/// its authorisation table.
pub fn assert_unauthenticated(gate: &Gate, method: &str, path: &str, body: Option<Value>) {
    let (code, _) = gate.json(method, path, None, body);
    assert!(
        code == 401 || code == 403,
        "an unauthenticated {method} {path} must be refused, got {code}"
    );
}

/// The component must IMPORT the capability the contract says it delegates to.
///
/// `gate_requires_capability` in the shell harness, which shelled to `wasm-tools`. The
/// reconciler already parses a component's surface, so this removes one more tool from
/// the list a worker must have.
///
/// Reads the UNCOMPOSED artifact, and that is the whole subtlety: composition
/// SATISFIES an import, so the composed `.wasm` no longer names it. Asserting on the
/// composed one reports "does not import quota:meter" about a component that imports
/// it correctly and has already been plugged.
///
/// The message matters more than the assertion: a part that hand-rolls authorisation
/// instead of calling `auth:identity/authorizer` passes every behavioural check and
/// has done the thing the contract exists to prevent.
pub fn requires_capability(crate_name: &str, interface: &str, why: &str) {
    let path = repo_root()
        .join("components/target/wasm32-wasip2/release")
        .join(format!("{}.wasm", crate_name.replace('-', "_")));
    let Ok(bytes) = std::fs::read(&path) else {
        panic!("cannot read {path:?} to check its imports — run `just build`");
    };
    let surface = comp_reconciler::plug::surface(&bytes)
        .unwrap_or_else(|e| panic!("cannot read the surface of {crate_name}: {e}"));
    let found = surface.imports.iter().chain(surface.host_imports.iter()).any(|i| i.starts_with(interface));
    assert!(
        found,
        "the component never calls {interface} — {why}\n\
         it imports: {}",
        surface.imports.iter().cloned().collect::<Vec<_>>().join(", ")
    );
}
