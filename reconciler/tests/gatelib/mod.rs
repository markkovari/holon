//! The gate harness in Rust: a gate judges a composed application over real HTTP against a real
//! host, needing only `comp-host` and a composed `.wasm` (artifacts this repository builds).
//!
//! Failure messages are preserved verbatim from the shell gates: ADR-0088 makes a gate's output the
//! next prompt a repair reads, so rewording changes what the loop reads.
//!
//! Each gate binds its own port because `cargo test` runs integration binaries in parallel (see
//! `harness/mod.rs`).

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// The root this was COMPILED in (not `COMP_REPO_ROOT`), through `comp-fleet`: the suites that
/// include this harness are in more than one package now, at different depths (docs/adr/0115).
pub fn repo_root() -> PathBuf {
    holon_fleet::fleet::root_from_manifest()
}

/// Where the components were built, from `$CARGO_TARGET_DIR` when something set it.
///
/// A goal run gives every check a SHARED cargo target dir (`goalrun/setup.rs`). `repo_root()` is
/// `CARGO_MANIFEST_DIR`, baked in at compile time, so resolving artifacts through it would judge
/// THIS machine's artifacts whatever the candidate wrote and pass for every branch.
pub fn components_target() -> PathBuf {
    match std::env::var("CARGO_TARGET_DIR") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => repo_root().join("components/target"),
    }
}

/// The host binary, from `$COMP_HOST` when something set it and from the checkout otherwise.
///
/// `holon goal run` materialises a candidate in a sandbox holding only the base tree and passes the
/// host binary as `$COMP_HOST`. Resolving by path instead made every ported gate find no binary,
/// SKIP, and (a skip returns `None`, which gates turn into `return`) pass.
///
/// `$COMP_PLUG` is deliberately NOT read: composition is a library call here (ADR-0087).
pub fn host_bin() -> PathBuf {
    match std::env::var("HOLON_HOST").or_else(|_| std::env::var("COMP_HOST")) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => repo_root().join("target/release/comp-host"),
    }
}

/// Whether a caller STATED where the host binary is. A cold checkout promised nothing and a skip is
/// kind; a goal run passes `$COMP_HOST` and CI sets it, and there a missing binary is the harness
/// being wrong and a skip would report a pass for something nothing executed.
pub fn host_was_stated() -> bool {
    std::env::var("HOLON_HOST")
        .or_else(|_| std::env::var("COMP_HOST"))
        .is_ok_and(|p| !p.trim().is_empty())
}

/// No host binary: a skip in a cold checkout, a panic when someone stated the path.
///
/// A gate that fails on a missing file in a checkout trains people to ignore gate failures, but
/// when a caller stated the path "nothing ran" must not reach the selector or CI as a pass.
fn no_host(app: &str, host: &std::path::Path) {
    if host_was_stated() {
        panic!(
            "[{app}] $COMP_HOST is set to `{}`, which does not exist. Something stated \
             where the host is, so this is a harness fault and not a cold checkout — \
             reporting a pass for a component nothing executed is worse than reporting \
             a failure.",
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
/// A hash of the app name collided across parallel test threads, and hash plus a counter still
/// collided across test binaries (`Address already in use` in CI). Asking the OS removes the guess.
/// The listener is dropped before the host binds, so there is a window; `serve_with` retries when
/// the host dies of it.
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

/// Compose the crate against what it imports, in this process, via `plug::compose_to` (ADR-0087).
/// Keyed by content, so a rerun against an unchanged tree reuses the artifact.
pub fn compose(crate_name: &str) -> Option<PathBuf> {
    use holon_fleet::plug::{compose_to, Catalog};
    let target = components_target();
    let dirs: Vec<PathBuf> =
        ["wasm32-wasip2/release", "wasm32-wasip2/debug"].iter().map(|d| target.join(d)).collect();
    let catalog = Catalog::scan(&dirs);
    // Same asymmetry as `no_host`: under a goal run an empty catalogue is the harness being wrong,
    // and a skip would reach the selector as a passing candidate.
    if catalog.is_empty() {
        assert!(
            !host_was_stated(),
            "[{crate_name}] the catalogue is empty while judging a candidate — the \
             check command must build the crate and its providers before this runs, \
             and a pass for a component nothing composed is worse than a failure"
        );
        eprintln!("SKIPPED [{crate_name}]: nothing is built — run `cargo xtask build --force`");
        return None;
    }
    if catalog.bytes(crate_name).is_none() {
        assert!(
            !host_was_stated(),
            "[{crate_name}] is not in the catalogue while judging a candidate — the \
             check command must build it before this runs"
        );
        eprintln!("SKIPPED [{crate_name}]: not built — run `cargo xtask build --force`");
        return None;
    }
    match compose_to(crate_name, &catalog, &target) {
        Ok(path) => Some(path),
        Err(e) => panic!("[{crate_name}] does not compose: {e}"),
    }
}

/// What a gate needs before it can run, or a reason it cannot. A loud skip rather than a failure.
pub fn artifacts(app: &str, composed: &str) -> Option<(PathBuf, PathBuf)> {
    let host = host_bin();
    let wasm = components_target().join(composed);
    if !host.exists() {
        eprintln!("SKIPPED [{app}]: no comp-host — cargo build --release --manifest-path host/Cargo.toml --bin comp-host");
        return None;
    }
    if !wasm.exists() {
        eprintln!("SKIPPED [{app}]: no {composed} — run `cargo xtask compose {app}`");
        return None;
    }
    Some((host, wasm))
}

impl Gate {
    /// Start the app on its own port and wait for it to answer.
    ///
    /// `config` is the `--config key=value` pairs the app needs; the two every gate sets are added here.
    /// As `compose_and_start`, plus the authorities the component may reach.
    ///
    /// `wasi:http` is DEFAULT-DENY by name and refuses loopback, so a gate that runs its own receiver
    /// has to say where it is.
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

    /// Compose `crate_name` here and serve it as `app`.
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
        // Three tries: a fresh port closes the small window between the OS naming a port and the host
        // binding it, and a third says the machine is out of ports.
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

        // `default-tenant` only. `allow-test-routes` is NOT added for everyone: adding it universally
        // stopped the moderation rate limiter limiting, so a gate asserting lock-out after three
        // submissions passed a fourth. Turning a flag on for everyone tests a configuration nothing ships.
        let mut args: Vec<String> =
            vec!["--app".into(), app.into(), "--config".into(), format!("default-tenant={app}")];
        for c in config {
            args.push("--config".into());
            args.push((*c).to_string());
        }
        // Unlike `allow-test-routes`, this changes nothing but whether `POST /test/token` exists, and
        // these gates log in through it. A gate that sets it (`test-tokens=off`) keeps its own value.
        if !config.iter().any(|c| c.starts_with("test-tokens=")) {
            args.push("--config".into());
            args.push("test-tokens=on".into());
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
            // Kept, not discarded: a `tail -3` of host output in CI was three lines of stack trace
            // with the cause above them.
            .stdout(Stdio::null())
            .stderr(log.try_clone().expect("clone the host log"))
            .spawn()
            .unwrap_or_else(|e| panic!("spawn comp-host: {e}"));

        let mut gate = Gate {
            child,
            base: format!("http://{addr}"),
            // TEN MINUTES: a route that calls a model holds the connection while it thinks (shell
            // gates pass `anthropic:timeout=540`). Fifteen seconds dropped the reply gate mid-answer
            // and reported a transport error.
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap(),
        };
        // SIXTY SECONDS, measured: a host on a loaded two-core runner can be slow, and both CI
        // failures were the shell harness's thirty-second wait expiring right after the heaviest gate.
        for _ in 0..1200 {
            if gate.client.get(format!("{}/health", gate.base)).send().is_ok() {
                return Ok(gate);
            }
            // A host that has already exited will never answer.
            if let Ok(Some(_)) = gate.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let said = std::fs::read_to_string(&log_path).unwrap_or_default();
        Err(if said.trim().is_empty() { format!("(nothing) on {addr}") } else { said })
    }

    /// (status, body). A non-2xx is a VALUE: most of a gate asserts a request is refused with a code.
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

    /// Extra headers, for routes that take one (`Idempotency-Key`: three treasury routes refuse a
    /// request without it).
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

    /// Read a document back through the test route, to assert the SHAPE that was written.
    pub fn stored(&self, kind: &str, id: &str) -> String {
        self.get(&format!("/test/{kind}/{id}"), None).1
    }

    /// The fixture. Returns the parsed body: a typed struct per app would be a second contract.
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

/// One top-level key out of a JSON body, empty when absent.
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

/// Every gate asserts this: a route behind auth refuses an anonymous caller. It catches the part
/// that skipped the contract's authorisation table.
pub fn assert_unauthenticated(gate: &Gate, method: &str, path: &str, body: Option<Value>) {
    let (code, _) = gate.json(method, path, None, body);
    assert!(
        code == 401 || code == 403,
        "an unauthenticated {method} {path} must be refused, got {code}"
    );
}

/// The component must IMPORT the capability the contract says it delegates to.
///
/// Reads the UNCOMPOSED artifact: composition SATISFIES an import, so the composed `.wasm` no
/// longer names it. The message matters: a part that hand-rolls authorisation instead of calling
/// `auth:identity/authorizer` passes every behavioural check.
pub fn requires_capability(crate_name: &str, interface: &str, why: &str) {
    let path = components_target()
        .join("wasm32-wasip2/release")
        .join(format!("{}.wasm", crate_name.replace('-', "_")));
    let Ok(bytes) = std::fs::read(&path) else {
        panic!("cannot read {path:?} to check its imports — run `cargo xtask build --force`");
    };
    let surface = holon_fleet::plug::surface(&bytes)
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
/// Delivery is not observable against a far end that always works (inline send, acked refusal and
/// retry of something delivered all look the same on the happy path), so the receiver records every
/// arrival and answers 500 while `break`n.
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
    /// Recorded even when refused: a refusal still arrived, and at-least-once delivery counts arrivals.
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
    /// The authority a component must be granted to reach this (`wasi:http` is default-deny and
    /// refuses loopback).
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

/// The TOTP code for a base32 secret, right now: RFC 6238 with the usual defaults (SHA-1,
/// thirty-second steps, six digits). SHA-1 is fixed by the RFC; any other code would be refused.
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

/// RFC 4648 base32, upper-cased, padding optional (what an `otpauth://` secret is).
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
/// MailHog cannot be installed on the CI runner (archived, predates modules), so the notification
/// fan-out had no coverage. SMTP is small enough to receive directly: greet, accept the envelope,
/// read until a lone dot, keep the message. The chain is component -> `mail:gateway-url` ->
/// `comp-mailrelay` -> this.
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
                                // Until a line that is exactly a dot; undo dot-stuffing as the sender applied it.
                                let mut body = String::new();
                                loop {
                                    let mut l = String::new();
                                    if reader.read_line(&mut l).unwrap_or(0) == 0 {
                                        break;
                                    }
                                    if l.trim_end_matches(['\r', '\n']) == "." {
                                        break;
                                    }
                                    body.push_str(l.strip_prefix("..").unwrap_or(&l));
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
    /// How many delivered messages contain `needle`.
    pub fn count_containing(&self, needle: &str) -> usize {
        self.messages.lock().expect("the mailbox").iter().filter(|m| m.contains(needle)).count()
    }

    /// The delivered messages that contain `needle`, whole. Two separate counts cannot tell an
    /// address in one message and a subject in another from the right message, which is the mix-up a
    /// fan-out produces.
    pub fn messages_containing(&self, needle: &str) -> Vec<String> {
        self.messages
            .lock()
            .expect("the mailbox")
            .iter()
            .filter(|m| m.contains(needle))
            .cloned()
            .collect()
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
        let bin = repo_root().join("target/release/comp-mailrelay");
        if !bin.exists() {
            eprintln!("SKIPPED: no comp-mailrelay — cargo build --release --bin comp-mailrelay");
            return None;
        }
        // Ask the OS for a free port: the relay takes an address rather than binding one.
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

/// Unix seconds as RFC 3339 UTC. `guestfmt::rfc3339` is a WASM-guest crate, not a dependency of the
/// reconciler; twenty lines here is cheaper than a native-to-guest dependency edge.
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
/// Five gates judge behaviour that only exists when a real model answered; `mock-provider` would
/// prove only that a canned string was plumbed through. So the REQUIREMENT is kept and these skip
/// without one, as the shell gates do.
///
/// A GET, which the shim answers 404 without spawning anything (probing `/v1/messages` would spend
/// a completion). `SHIM_URL` overrides; `.comp/csatapaci.env` documents the ways to have one.
pub struct Shim {
    url: String,
}

impl Shim {
    /// The shim, or a loud skip, and a PANIC when somebody named one that is not there.
    ///
    /// Same asymmetry as `host_bin`/`no_host`: nobody promised a model on the default port, but
    /// `SHIM_URL` is a statement, and an address that does not answer must fail rather than return
    /// `None` and print `ok`.
    ///
    /// This does NOT make the model gates run in CI (no shim there; they still skip).
    pub fn probe(gate_name: &str) -> Option<Self> {
        let stated = std::env::var("SHIM_URL").ok().filter(|u| !u.trim().is_empty());
        let url = stated.clone().unwrap_or_else(|| "http://127.0.0.1:8787".into());
        let alive = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()
            .and_then(|c| c.get(format!("{url}/")).send().ok())
            .map(|r| r.status().as_u16() == 404)
            .unwrap_or(false);
        if !alive {
            assert!(
                stated.is_none(),
                "[{gate_name}] SHIM_URL is set to `{url}` and nothing answers there. \
                 Something stated where the model is, so this is a harness fault and not \
                 a machine without one — reporting a pass for a gate that never called a \
                 model is worse than reporting a failure."
            );
            eprintln!(
                "SKIPPED [{gate_name}]: no model shim at {url} — start one with \
                 `node tools/openai-shim.mjs` (a local mlx server) or \
                 `node tools/claude-shim.mjs` (a subscription), or set SHIM_URL"
            );
            return None;
        }
        Some(Self { url })
    }

    /// `anthropic:base-url` and a long timeout: a thinking model on a local box takes minutes.
    pub fn config(&self) -> Vec<String> {
        vec![format!("anthropic:base-url={}", self.url), "anthropic:timeout=540".into()]
    }

    /// The authority the component must be granted to reach it.
    pub fn egress(&self) -> String {
        self.url.trim_start_matches("http://").trim_start_matches("https://").to_string()
    }
}
