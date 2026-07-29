//! E2E for the pulse chat room (REALTIME.md) as ONE composed wasm HTTP component
//! on the native Rust host. Rung 1: post + history. Rung 2 (the headline): a
//! message posted by one request is delivered LIVE over a separate, held-open
//! Server-Sent-Events connection — real server push on wasip2.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3025";

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

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let r = ureq::request(method, &url);
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

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/pulse_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-pulse`)");
    assert!(component.exists(), "composed wasm missing (just compose-pulse)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "pulse")
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
    panic!("pulse host did not start");
}

#[test]
fn post_history_and_live_sse() {
    let _host = start_host();

    // ===== rung 1: post + history ===========================================
    let (status, m) = req("POST", "/api/rooms/lobby/messages", Some(json!({"user": "ada", "text": "first!"})));
    assert_eq!(status, 201, "post: {m}");
    assert_eq!(m["user"], "ada");
    assert_eq!(m["text"], "first!");

    let (status, page) = req("GET", "/api/rooms/lobby/messages?after=-1", None);
    assert_eq!(status, 200, "history: {page}");
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["text"], "first!");
    let cursor = page["cursor"].as_i64().unwrap();

    // a second message; catch-up from the cursor returns only the new one.
    req("POST", "/api/rooms/lobby/messages", Some(json!({"user": "bob", "text": "second"})));
    let (_, page) = req("GET", &format!("/api/rooms/lobby/messages?after={cursor}"), None);
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "only messages after the cursor: {page}");
    assert_eq!(msgs[0]["text"], "second");

    // rooms are isolated
    let (_, page) = req("GET", "/api/rooms/other/messages?after=-1", None);
    assert_eq!(page["messages"].as_array().unwrap().len(), 0, "other room is empty");

    // ===== rung 2: LIVE SSE — the headline ==================================
    // A reader holds a GET /events connection open; a separate POST must reach it.
    let found = Arc::new(AtomicBool::new(false));
    let f = found.clone();
    let url = format!("{}/api/rooms/live/events", base());
    let reader = std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout_read(Duration::from_secs(2)).build();
        let Ok(resp) = agent.get(&url).call() else { return };
        let mut buf = BufReader::new(resp.into_reader());
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            match buf.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.starts_with("data:") && line.contains("live-hello") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {} // read timeout tick — the ": ping" heartbeats keep us moving
            }
        }
    });

    // let the reader connect and pin its cursor at "now" before we post.
    std::thread::sleep(Duration::from_millis(900));
    let (status, _) = req("POST", "/api/rooms/live/messages", Some(json!({"user": "carol", "text": "live-hello"})));
    assert_eq!(status, 201);

    reader.join().unwrap();
    assert!(
        found.load(Ordering::SeqCst),
        "the live SSE connection should have received the posted message as a data: frame"
    );
}
