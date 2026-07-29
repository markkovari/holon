//! E2E for the project tracker (TRACK.md) as ONE composed wasm HTTP component
//! on the native Rust host — the biggest composition in the repo (~14 contracts).
//! Drives all five axes: auth + RBAC (admin creates a project, a member writes,
//! a non-member is 403), the issue lifecycle over the fsm, full-text search, a
//! live SSE activity frame, the background stale-sweep tick, and the AI thread
//! summary.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3036";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://{ADDR}")
}

/// Authenticated (or anonymous) JSON request. `token` empty = no auth header.
fn req(method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
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

fn register(email: &str, role: Option<&str>) {
    let mut b = json!({"email": email, "password": "pw12345678"});
    if let Some(r) = role {
        b["role"] = json!(r);
    }
    let (s, _) = req("POST", "/auth/register", "", Some(b));
    assert!(s == 201 || s == 409, "register {email}: {s}");
}

fn login(email: &str) -> String {
    let (s, t) = req("POST", "/auth/login", "", Some(json!({"email": email, "password": "pw12345678"})));
    assert_eq!(s, 200, "login {email}: {t}");
    t["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/track_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-track`)");
    assert!(component.exists(), "composed wasm missing (just compose-track)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "track")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("track host did not start");
}

#[test]
fn five_axes() {
    let _host = start_host();

    // --- auth + RBAC ---------------------------------------------------------
    register("admin@track.io", Some("admin"));
    register("carol@track.io", None); // a plain member account
    register("mallory@track.io", None); // never added to the project
    let admin = login("admin@track.io");
    let carol = login("carol@track.io");
    let mallory = login("mallory@track.io");

    // a non-admin cannot create a project.
    let (s, _) = req("POST", "/api/projects", &carol, Some(json!({"key": "X", "name": "nope"})));
    assert_eq!(s, 403, "non-admin must not create a project");

    // admin creates a project (and is its lead).
    let (s, proj) = req("POST", "/api/projects", &admin, Some(json!({"key": "ENG", "name": "Engineering"})));
    assert_eq!(s, 201, "create project: {proj}");
    let pid = proj["id"].as_str().unwrap().to_string();

    // carol is added as a member; mallory is not.
    let (s, _) = req("POST", &format!("/api/projects/{pid}/members"), &admin, Some(json!({"subject": subject(&carol), "role": "member"})));
    assert_eq!(s, 201, "add member");

    // --- write axis + membership ABAC ---------------------------------------
    // mallory (not a member) is forbidden from filing an issue.
    let (s, _) = req("POST", "/api/issues", &mallory, Some(json!({"project": pid, "title": "sneaky"})));
    assert_eq!(s, 403, "non-member must not write issues");

    // carol (a member) can file one.
    let (s, iss) = req("POST", "/api/issues", &carol, Some(json!({"project": pid, "title": "Fix the login bug", "body": "Token expiry uses < not <=", "label": "bug"})));
    assert_eq!(s, 201, "member creates issue: {iss}");
    let iid = iss["id"].as_str().unwrap().to_string();
    assert_eq!(iss["ref"], "ENG-1", "per-project issue number");
    assert_eq!(iss["status"], "backlog");

    // --- lifecycle (fsm) -----------------------------------------------------
    for (event, expect) in [("start", "todo"), ("begin", "in_progress"), ("finish", "done")] {
        let (s, m) = req("POST", &format!("/api/issues/{iid}/move"), &carol, Some(json!({"event": event})));
        assert_eq!(s, 200, "move {event}: {m}");
        assert_eq!(m["status"], expect, "after {event}");
    }
    // an illegal transition is a 409.
    let (s, _) = req("POST", &format!("/api/issues/{iid}/move"), &carol, Some(json!({"event": "begin"})));
    assert_eq!(s, 409, "illegal transition from done");

    // --- comment + AI summary ------------------------------------------------
    let (s, _) = req("POST", &format!("/api/issues/{iid}/comments"), &carol, Some(json!({"body": "Off-by-one in the comparison; patch incoming."})));
    assert_eq!(s, 201, "add comment");
    let (s, sum) = req("POST", &format!("/api/issues/{iid}/summarize"), &carol, None);
    assert_eq!(s, 200, "ai summarize: {sum}");
    assert!(!sum["summary"].as_str().unwrap_or("").is_empty(), "AI returns a non-empty summary");
    assert_eq!(sum["comments"].as_u64().unwrap(), 1);

    // --- read axis (search) --------------------------------------------------
    let (s, res) = req("GET", "/api/search?q=login+bug", &carol, None);
    assert_eq!(s, 200, "search: {res}");
    let hits = res["hits"].as_array().unwrap();
    assert!(hits.iter().any(|h| h["ref"] == "ENG-1"), "search finds ENG-1: {res}");

    // --- background sweep (tick) --------------------------------------------
    // a fresh in_progress issue is NOT stale, so the sweep flags nothing.
    let (s, sweep) = req("POST", "/api/tick", &admin, None);
    assert_eq!(s, 200, "tick: {sweep}");
    assert_eq!(sweep["flagged"].as_u64().unwrap(), 0, "nothing stale yet");

    // --- stream axis (SSE) ---------------------------------------------------
    // connect an SSE reader, then create an issue and assert the frame arrives.
    let stream_url = format!("{}/api/stream", base());
    let handle = std::thread::spawn(move || {
        let resp = ureq::get(&stream_url).timeout(Duration::from_secs(6)).call().expect("sse connect");
        let mut reader = BufReader::new(resp.into_reader());
        let mut line = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return false;
            }
            if line.starts_with("data:") && line.contains("issue.created") {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
        }
    });
    std::thread::sleep(Duration::from_millis(600)); // let the reader connect + set its cursor
    let (s, _) = req("POST", "/api/issues", &carol, Some(json!({"project": pid, "title": "second issue for the feed"})));
    assert_eq!(s, 201, "create issue to drive the feed");
    assert!(handle.join().unwrap(), "an issue.created frame must reach the SSE feed");
}

/// Extract the subject from a session by calling /auth/me with the token.
fn subject(token: &str) -> String {
    let (_, me) = req("GET", "/auth/me", token, None);
    me["subject"].as_str().unwrap().to_string()
}
