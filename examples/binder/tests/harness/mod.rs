#![allow(dead_code)]
//! The host the e2e suites drive, and the client they drive it with.
//!
//! Extracted when `features.rs` needed the same thing `binder.rs` had. One copy,
//! because two harnesses that drift is how a suite starts passing against a host
//! the other suite would fail.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// One port per test, and per scenario. `cargo test` runs a file's tests as THREADS
/// in one process, so a single fixed address means the second to start finds the
/// first one's host — and, worse, its collection, which turns every total into a
/// multiple of the right answer.
pub const PORTS: &[u16] = &[3211, 3212, 3213];

thread_local! {
    static ADDR_CELL: std::cell::RefCell<String> =
        std::cell::RefCell::new(format!("127.0.0.1:{}", PORTS[0]));
}

pub fn addr() -> String {
    ADDR_CELL.with(|a| a.borrow().clone())
}
pub const DAY: u64 = 86_400;

pub struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    auth_req(method, path, body, "")
}

/// A raw file upload. The bulk import takes the FILE as the body, not JSON with a
/// base64 blob in it — a spreadsheet is already bytes, and wrapping it costs a third
/// more of them for nothing.
pub fn upload(path: &str, bytes: &[u8], token: &str) -> (u16, Value) {
    let url = format!("http://{}{path}", addr());
    let result = ureq::post(&url)
        .set("authorization", &format!("Bearer {token}"))
        .set("content-type", "application/octet-stream")
        .send_bytes(bytes);
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("POST {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

pub fn auth_req(method: &str, path: &str, body: Option<Value>, token: &str) -> (u16, Value) {
    let url = format!("http://{}{path}", addr());
    let mut r = ureq::request(method, &url);
    if !token.is_empty() {
        r = r.set("authorization", &format!("Bearer {token}"));
    }
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

pub fn start_host_on(port: u16) -> HostGuard {
    ADDR_CELL.with(|a| *a.borrow_mut() = format!("127.0.0.1:{port}"));
    // Refuse to run against a host this test did not start. A leaked `comp-host`
    // from an interrupted run keeps the port AND its in-memory collection, so the
    // events below land on top of an earlier run's and every total comes out a
    // multiple of the right answer — which reads as broken arithmetic in the
    // capability rather than as a stale process.
    let bind = addr();
    match std::net::TcpListener::bind(&bind) {
        Ok(l) => drop(l),
        Err(e) => panic!(
            "something is already listening on {bind} ({e}). A comp-host from an \
             earlier run is still up and its store is not empty — `pkill -f comp-host`"
        ),
    }

    let root = repo_root();
    let bin = root.join("host/target/release/comp-host");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-binder`)");

    // The DERIVED composition (ADR-0087), asked for by name rather than by a path a
    // recipe had to keep in step with the digest.
    let plug = root.join("reconciler/target/release/comp-plug");
    assert!(plug.exists(), "comp-plug not built (run `just e2e-binder`)");
    // From the repo root: `comp-plug` resolves a component by name against
    // `components/`, and the test's own cwd is this crate.
    let composed =
        Command::new(&plug).arg("binder-domain").current_dir(&root).output().expect("comp-plug");
    let composed = String::from_utf8_lossy(&composed.stdout).trim().to_string();
    assert!(!composed.is_empty(), "comp-plug produced no artifact — is binder-domain built?");

    let mut child = Command::new(&bin)
        .args([
            "--app", "binder", "--component", &composed, "--addr", &bind,
            // auth-guard reads its tenant from config; without it every register
            // lands in a different tenant from every login.
            "--config", "default-tenant=binder",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("comp-host");

    // Wait for the line the host prints when it is actually serving, rather than
    // sleeping and hoping: a fixed sleep is the difference between a suite that is
    // flaky on a loaded machine and one that is not.
    // BOTH streams: the host writes its banner to stdout, and watching only stderr
    // is a 30-second timeout that looks exactly like a host that failed to start.
    let (tx, rx) = std::sync::mpsc::channel();
    for stream in [
        Box::new(child.stdout.take().expect("stdout")) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().expect("stderr")),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if line.contains("serving") {
                    let _ = tx.send(());
                }
            }
        });
    }
    rx.recv_timeout(Duration::from_secs(30)).expect("the host never reported serving");
    HostGuard(child)
}

/// One test, because the state is one collection and splitting it would need either a
/// fixture per test or an order dependency between them.
/// A signed-in caller. Holds the token so a scenario does not have to say so.
pub struct Client {
    token: String,
}

impl Client {
    pub fn new() -> Self {
        Client { token: String::new() }
    }

    /// Register, then log in. Registering twice is harmless, so the result is
    /// ignored and the token always comes from the login — the CLI does the same.
    pub fn sign_in(&mut self, email: &str) {
        let creds = serde_json::json!({ "email": email, "password": "pw12345678" });
        let _ = req("POST", "/api/register", Some(creds.clone()));
        let (status, session) = req("POST", "/api/login", Some(creds));
        assert_eq!(status, 200, "login as {email}: {session}");
        self.token =
            session["access_token"].as_str().expect("a token in the session").to_string();
    }

    pub fn get(&self, path: &str, authenticated: bool) -> (u16, Value) {
        let token = if authenticated { self.token.as_str() } else { "" };
        auth_req("GET", path, None, token)
    }

    pub fn post(&self, path: &str, body: Value) -> (u16, Value) {
        auth_req("POST", path, Some(body), &self.token)
    }

    pub fn upload_to(&self, path: &str, bytes: &[u8]) -> (u16, Value) {
        upload(path, bytes, &self.token)
    }

    /// Named `upload` at the call site because that is what the step says.
    pub fn upload(&self, path: &str, bytes: &[u8]) -> (u16, Value) {
        self.upload_to(path, bytes)
    }
}

