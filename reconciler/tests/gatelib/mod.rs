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

/// The host binary, from `$COMP_HOST` when something set it and from the checkout
/// otherwise.
///
/// This is the whole reason a ported gate could not gate a GOAL RUN, which is what
/// the ports were for. `holon goal run` materialises a candidate in a sandbox that
/// holds the base tree and nothing else, and passes the two binaries it cannot
/// contain as `$COMP_HOST` and `$COMP_PLUG` — `components/gate-lib.sh` says so in as
/// many words, and the shell gates read them. The Rust harness resolved the host by
/// path instead, so under a goal every ported gate found no binary, SKIPPED, and a
/// skip returns `None`, which every gate turns into `return` — a pass. Thirty-one
/// gates that pass because nothing was there to judge with is the empty-corpus
/// candidate wearing a different hat, and it is why every goal spec in `.comp/goals/`
/// still points at the shell copies of gates that were ported months ago.
///
/// `$COMP_PLUG` is deliberately NOT read: composition is a library call here
/// (ADR-0087), so this harness needs no plug binary at all.
pub fn host_bin() -> PathBuf {
    match std::env::var("COMP_HOST") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => repo_root().join("host/target/release/comp-host"),
    }
}

/// Whether this gate is judging a candidate rather than a checkout.
///
/// Under a goal run a missing binary is not "you have not built yet", it is the
/// harness being wrong — and answering that with a skip would report a pass for a
/// candidate nothing executed.
pub fn under_a_goal_run() -> bool {
    std::env::var("COMP_HOST").is_ok_and(|p| !p.trim().is_empty())
}

/// No host binary: a skip in a checkout, a panic under a goal.
///
/// The asymmetry is the point. In a checkout, nothing has been broken by not having
/// built yet, and a gate that fails for a missing file trains people to ignore gate
/// failures. Under a goal run there is a candidate being judged and a score being
/// written down, and "nothing ran" must not arrive at the selector as a pass.
fn no_host(app: &str, host: &std::path::Path) {
    if under_a_goal_run() {
        panic!(
            "[{app}] $COMP_HOST is set to `{}`, which does not exist. A candidate is \
             being judged, so this is a harness fault and not a skip — reporting a pass \
             for a component nothing executed is worse than reporting a failure.",
            host.display()
        );
    }
    eprintln!(
        "SKIPPED [{app}]: no comp-host at {} — cargo build --release --manifest-path host/Cargo.toml --bin comp-host",
        host.display()
    );
}

/// A port the OS says is free.
///
/// Two earlier versions of this were wrong, and the second failed in CI as
/// `Error: Address already in use (os error 98)` — which is only visible because this
/// harness keeps the host's stderr and prints it, where the shell printed `tail -3` and
/// showed three lines of stack trace.
///
///   1. a hash of the APP name. `cargo test` runs the tests in one binary on parallel
///      threads, so three triage gates named one port, started three hosts on it, and
///      two failed while the third passed.
///   2. that hash plus a per-process counter. Better, and still a guess: twenty-one
///      test binaries each pick a base from 18000 slots and count up from it, so two
///      bases landing within a few of each other collide.
///
/// Asking the OS removes the guess. The listener is dropped before the host binds, so
/// there is a window — hence `serve_with` retries when the host dies of it, which is a
/// race that closes rather than a hash that does not.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS has no free port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
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
    // Same asymmetry as `no_host`, and for the same reason: under a goal run the
    // check command has already built what it needs, so an empty catalogue here is
    // the harness being wrong rather than a checkout being cold — and a skip would
    // reach the selector as a candidate that passed.
    if catalog.is_empty() {
        assert!(
            !under_a_goal_run(),
            "[{crate_name}] the catalogue is empty while judging a candidate — the \
             check command must build the crate and its providers before this runs, \
             and a pass for a component nothing composed is worse than a failure"
        );
        eprintln!("SKIPPED [{crate_name}]: nothing is built — run `just build`");
        return None;
    }
    if catalog.bytes(crate_name).is_none() {
        assert!(
            !under_a_goal_run(),
            "[{crate_name}] is not in the catalogue while judging a candidate — the \
             check command must build it before this runs"
        );
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
    let host = host_bin();
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
    /// As `compose_and_start`, plus the authorities the component may reach.
    ///
    /// `wasi:http` is DEFAULT-DENY by name and refuses loopback twice over, so a gate
    /// that runs its own receiver has to say where it is — which is the point of the
    /// deny rather than an inconvenience of it.
    pub fn compose_and_start_with_egress(
        app: &str,
        crate_name: &str,
        config: &[&str],
        egress: &[&str],
    ) -> Option<Self> {
        let wasm = compose(crate_name)?;
        let host = host_bin();
        if !host.exists() {
            no_host(app, &host);
            return None;
        }
        Some(Self::serve_with(app, &host, &wasm, config, egress))
    }

    /// Compose `crate_name` here and serve it as `app`. The shape every gate wants:
    /// nothing has to have run `just compose-…` first.
    pub fn compose_and_start(app: &str, crate_name: &str, config: &[&str]) -> Option<Self> {
        let wasm = compose(crate_name)?;
        let host = host_bin();
        if !host.exists() {
            no_host(app, &host);
            return None;
        }
        Some(Self::serve(app, &host, &wasm, config))
    }

    pub fn start(app: &str, composed: &str, config: &[&str]) -> Option<Self> {
        let (host, wasm) = artifacts(app, composed)?;
        Some(Self::serve(app, &host, &wasm, config))
    }

    fn serve(app: &str, host: &std::path::Path, wasm: &std::path::Path, config: &[&str]) -> Self {
        Self::serve_with(app, host, wasm, config, &[])
    }

    fn serve_with(
        app: &str,
        host: &std::path::Path,
        wasm: &std::path::Path,
        config: &[&str],
        egress: &[&str],
    ) -> Self {
        // Three tries. The window between the OS naming a free port and the host
        // binding it is small and real; a second attempt on a fresh port closes it,
        // and a third says the machine is out of ports rather than this being flaky.
        for attempt in 1..=3 {
            match Self::serve_once(app, host, wasm, config, egress) {
                Ok(gate) => return gate,
                Err(said) if said.contains("Address already in use") && attempt < 3 => {
                    eprintln!("[{app}] port taken between choosing and binding, retrying");
                }
                Err(said) => {
                    panic!("[{app}] comp-host never answered /health.\nThe host said:\n{said}")
                }
            }
        }
        unreachable!("the loop above either returns or panics")
    }

    fn serve_once(
        app: &str,
        host: &std::path::Path,
        wasm: &std::path::Path,
        config: &[&str],
        egress: &[&str],
    ) -> Result<Self, String> {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");

        // `default-tenant` only. `allow-test-routes` is NOT added here even though every
        // gate uses `/test/…`: only `events-domain`'s shell lib sets it, the others reach
        // their fixtures without it, and adding it universally changed behaviour — the
        // moderation rate limiter stopped limiting, so a gate that asserts a subject is
        // locked out after three submissions passed a fourth. A harness that turns a flag
        // on for everyone is a harness that tests a configuration nothing ships.
        let mut args: Vec<String> =
            vec!["--app".into(), app.into(), "--config".into(), format!("default-tenant={app}")];
        for c in config {
            args.push("--config".into());
            args.push((*c).to_string());
        }
        for e in egress {
            args.push("--egress".into());
            args.push((*e).to_string());
        }
        if !egress.is_empty() {
            args.push("--allow-private-egress".into());
        }
        args.extend([
            "--component".into(),
            wasm.to_string_lossy().into_owned(),
            "--addr".into(),
            addr.clone(),
        ]);

        let log_path = std::env::temp_dir().join(format!("holon-gate-{app}-{port}.log"));
        let log = std::fs::File::create(&log_path).expect("create the host log");

        let child = Command::new(host)
            .args(&args)
            // Kept, not discarded. The shell harness sends the host's output to a temp
            // file and prints `tail -3` of it on failure — which, when a host actually
            // failed in CI, was three lines of stack trace with the cause above them.
            .stdout(Stdio::null())
            .stderr(log.try_clone().expect("clone the host log"))
            .spawn()
            .unwrap_or_else(|e| panic!("spawn comp-host: {e}"));

        let mut gate = Gate {
            child,
            base: format!("http://{addr}"),
            // TEN MINUTES, and not because a gate is slow. A route that calls a model
            // holds the connection while the model thinks, and the shell gates pass
            // `anthropic:timeout=540` for exactly that. Fifteen seconds — what this was
            // — dropped the reply gate mid-answer and reported it as a transport error,
            // which reads as the app being broken rather than the client giving up.
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap(),
        };
        // SIXTY SECONDS, and the number is measured rather than picked. A host on a
        // loaded two-core runner can take a while to come up, and both CI failures this
        // shape produced were the shell harness's THIRTY-second wait expiring, in the
        // gate that happened to run right after the heaviest one. Ten seconds — what
        // this waited first — would have been worse.
        for _ in 0..1200 {
            if gate.client.get(format!("{}/health", gate.base)).send().is_ok() {
                return Ok(gate);
            }
            // A host that has already exited will never answer; do not wait out the
            // full minute for one that is gone.
            if let Ok(Some(_)) = gate.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // What the host said, not the last three lines of it.
        let said = std::fs::read_to_string(&log_path).unwrap_or_default();
        Err(if said.trim().is_empty() { format!("(nothing) on {addr}") } else { said })
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

    /// Extra headers, for the routes that take one. `Idempotency-Key` is the reason
    /// this exists: three treasury routes refuse a request without it, and a gate that
    /// cannot send one cannot judge them.
    pub fn with_headers(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, String) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut r = self.client.request(m, format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("authorization", format!("Bearer {t}"));
        }
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        if let Some(b) = body {
            r = r.header("content-type", "application/json").body(b.to_string());
        }
        let resp = r.send().unwrap_or_else(|e| panic!("{method} {path}: transport error: {e}"));
        (resp.status().as_u16(), resp.text().unwrap_or_default())
    }

    pub fn json(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, String) {
        self.send(
            method,
            path,
            token,
            body.map(|b| ("application/json", b.to_string().into_bytes())),
        )
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
        serde_json::from_str(&raw)
            .unwrap_or_else(|_| panic!("the fixture did not come back as JSON: {raw}"))
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
    let found =
        surface.imports.iter().chain(surface.host_imports.iter()).any(|i| i.starts_with(interface));
    assert!(
        found,
        "the component never calls {interface} — {why}\n\
         it imports: {}",
        surface.imports.iter().cloned().collect::<Vec<_>>().join(", ")
    );
}

/// A recording HTTP receiver the gate runs itself, which can be broken on purpose.
///
/// Delivery is the whole subject of some of these apps and none of it is observable
/// against a far end that always works: an app that sends inline, one that acks a
/// refusal, and one that retries something already delivered all look identical on the
/// happy path. So a gate runs its own receiver, records every arrival, and answers 500
/// while it is `break`n — which is how "the far end refused" becomes a thing a test can
/// arrange.
///
/// The shell version is twenty lines of `python3 -m http.server` in a heredoc, writing
/// JSON lines to a temp file that the gate then re-reads and re-parses. Here the
/// arrivals are a `Vec` behind a mutex, which is the same thing without the file, the
/// interpreter, or the "grep -c prints 0 AND exits 1 on an empty file" footnote the
/// shell needed.
pub struct Sink {
    port: u16,
    arrivals: std::sync::Arc<std::sync::Mutex<Vec<Arrival>>>,
    failing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Dropping this stops the accept loop.
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone, Debug)]
pub struct Arrival {
    pub path: String,
    pub body: String,
    /// Recorded even when refused: "how many times did this arrive" is the question
    /// at-least-once delivery is about, and a refusal still arrived.
    pub refused: bool,
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        // Unblock the accept loop so the thread notices and exits.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Sink {
    pub fn start() -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a sink");
        let port = listener.local_addr().expect("sink address").port();
        let arrivals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let failing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let (a, f, s) = (arrivals.clone(), failing.clone(), shutdown.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone the stream"));

                // Request line, then headers, then exactly content-length bytes.
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                if len > 0 {
                    let _ = std::io::Read::read_exact(&mut reader, &mut body);
                }

                let refused = f.load(Ordering::SeqCst);
                a.lock().expect("the sink log").push(Arrival {
                    path,
                    body: String::from_utf8_lossy(&body).into_owned(),
                    refused,
                });
                let status = if refused { "500 Internal Server Error" } else { "200 OK" };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{{\"ok\":true}}"
                );
                let _ = stream.flush();
            }
        });

        Self { port, arrivals, failing, shutdown }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }
    /// The authority a component must be granted to reach this. `wasi:http` is
    /// default-deny by name and refuses loopback twice over.
    pub fn egress(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    pub fn arrivals(&self) -> Vec<Arrival> {
        self.arrivals.lock().expect("the sink log").clone()
    }
    pub fn deliveries(&self) -> usize {
        self.arrivals().len()
    }
    pub fn forget(&self) {
        self.arrivals.lock().expect("the sink log").clear();
    }
    pub fn fail(&self) {
        self.failing.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn repair(&self) {
        self.failing.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The TOTP code for a base32 secret, right now — RFC 6238 with the defaults every
/// authenticator app uses: SHA-1, thirty-second steps, six digits.
///
/// The shell gate does this with `python3`'s `hmac` and `hashlib.sha1`. SHA-1 is not a
/// choice here: RFC 6238 fixes it, and a code computed any other way is not one the
/// component will accept.
pub fn totp_now(secret_base32: &str) -> String {
    use hmac::{Mac, SimpleHmac};
    let key = base32_decode(secret_base32);
    let counter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is before 1970")
        .as_secs()
        / 30;
    let mut mac =
        SimpleHmac::<sha1::Sha1>::new_from_slice(&key).expect("hmac accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let code = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]) % 1_000_000;
    format!("{code:06}")
}

/// RFC 4648 base32, upper-cased, padding optional — what an `otpauth://` secret is.
fn base32_decode(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in s.trim().to_ascii_uppercase().bytes().filter(|c| *c != b'=') {
        let Some(v) = A.iter().position(|a| *a == c) else { continue };
        buf = (buf << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// An SMTP receiver that keeps what it was sent.
///
/// The shell gate runs MailHog for this, and CI cannot: `go install
/// github.com/mailhog/MailHog@latest` fails on the runner — the project is archived
/// and predates modules — so `e2e-reminders.sh` is skipped there and the notification
/// fan-out, the one thing in that app that talks to the outside, has no coverage.
///
/// SMTP is small enough to receive directly: greet, accept the envelope, read until a
/// lone dot, keep the message. Only what `comp-mailrelay` sends, which is the same
/// principle as `guestfmt` — a receiver that needs a mail server to state its claim is
/// testing the mail server.
///
/// The chain is unchanged: the component posts to `mail:gateway-url`, `comp-mailrelay`
/// turns that into SMTP, and this is what listens. Two of those three are already
/// artifacts this repository builds; this makes it three.
pub struct MailSink {
    port: u16,
    messages: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for MailSink {
    fn drop(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl MailSink {
    pub fn start() -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an smtp sink");
        let port = listener.local_addr().expect("smtp address").port();
        let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let (m, s) = (messages.clone(), shutdown.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let m = m.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let _ = write!(stream, "220 holon test sink\r\n");
                    let _ = stream.flush();
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let verb =
                            line.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                        match verb.as_str() {
                            "EHLO" | "HELO" => {
                                let _ = write!(stream, "250 ok\r\n");
                            }
                            "MAIL" | "RCPT" | "RSET" | "NOOP" => {
                                let _ = write!(stream, "250 ok\r\n");
                            }
                            "DATA" => {
                                let _ = write!(stream, "354 send it\r\n");
                                let _ = stream.flush();
                                // Until a line that is exactly a dot. Dot-stuffing is
                                // undone the way the sender applied it.
                                let mut body = String::new();
                                loop {
                                    let mut l = String::new();
                                    if reader.read_line(&mut l).unwrap_or(0) == 0 {
                                        break;
                                    }
                                    if l.trim_end_matches(['\r', '\n']) == "." {
                                        break;
                                    }
                                    body.push_str(l.strip_prefix("..").map(|r| r).unwrap_or(&l));
                                }
                                m.lock().expect("the mailbox").push(body);
                                let _ = write!(stream, "250 queued\r\n");
                            }
                            "QUIT" => {
                                let _ = write!(stream, "221 bye\r\n");
                                let _ = stream.flush();
                                return;
                            }
                            _ => {
                                let _ = write!(stream, "250 ok\r\n");
                            }
                        }
                        let _ = stream.flush();
                    }
                });
            }
        });

        Self { port, messages, shutdown }
    }

    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    /// How many delivered messages contain `needle` — `mail_count_containing` in the
    /// shell, which asked MailHog's HTTP API and counted with `python3`.
    pub fn count_containing(&self, needle: &str) -> usize {
        self.messages.lock().expect("the mailbox").iter().filter(|m| m.contains(needle)).count()
    }
}

/// `comp-mailrelay`, the HTTP-to-SMTP bridge the component actually posts to.
///
/// Returns the URL to give `mail:gateway-url`, and a guard that stops it.
pub struct MailRelay {
    child: Child,
    port: u16,
}

impl Drop for MailRelay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl MailRelay {
    pub fn start(smtp: &str) -> Option<Self> {
        let bin = repo_root().join("reconciler/target/release/comp-mailrelay");
        if !bin.exists() {
            eprintln!("SKIPPED: no comp-mailrelay — cargo build --release --bin comp-mailrelay");
            return None;
        }
        // Ask the OS for a free port, then hand the number over: the relay takes an
        // address rather than binding one it reports back.
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let child = Command::new(&bin)
            .arg(format!("127.0.0.1:{port}"))
            .arg(smtp)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        std::thread::sleep(Duration::from_millis(200));
        Some(Self { child, port })
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
    pub fn egress(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// Unix seconds as RFC 3339 UTC.
///
/// `guestfmt::rfc3339` is the same arithmetic, and is a WASM-guest crate rather than a
/// dependency of the reconciler; twenty lines of Howard Hinnant here is cheaper than a
/// dependency edge from a native workspace to a guest one.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// The model endpoint a gate needs, or a loud skip.
///
/// Five gates judge behaviour that only exists when a real model answered: whether the
/// reply is about the question, whether a verdict cites the rule it applied. Pointing
/// them at `mock-provider` would prove the app plumbed a canned string through, which
/// is a different test wearing the same name — so the REQUIREMENT is ported, not
/// removed, and these skip without one exactly as the shell gates do.
///
/// A GET, which the shim answers 404 without spawning anything. Probing `/v1/messages`
/// for real would spend a completion on liveness.
///
/// `SHIM_URL` overrides, and `.comp/csatapaci.env` documents the two ways to have one:
/// `just openai-shim` in front of a local mlx server, or `just claude-shim` in front of
/// a subscription.
pub struct Shim {
    url: String,
}

impl Shim {
    pub fn probe(gate_name: &str) -> Option<Self> {
        let url = std::env::var("SHIM_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".into());
        let alive = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()
            .and_then(|c| c.get(format!("{url}/")).send().ok())
            .map(|r| r.status().as_u16() == 404)
            .unwrap_or(false);
        if !alive {
            eprintln!(
                "SKIPPED [{gate_name}]: no model shim at {url} — start one with \
                 `just openai-shim` (a local mlx server) or `just claude-shim` (a \
                 subscription), or set SHIM_URL"
            );
            return None;
        }
        Some(Self { url })
    }

    /// `anthropic:base-url` and a long timeout: a thinking model on a local box takes
    /// minutes, and 540 is what the shell gates pass.
    pub fn config(&self) -> Vec<String> {
        vec![format!("anthropic:base-url={}", self.url), "anthropic:timeout=540".into()]
    }

    /// The authority the component must be granted to reach it.
    pub fn egress(&self) -> String {
        self.url.trim_start_matches("http://").trim_start_matches("https://").to_string()
    }
}
