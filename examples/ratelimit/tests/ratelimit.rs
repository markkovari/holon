//! E2E for the throttle wall (docs/apps/RATELIMIT.md) as ONE composed wasm HTTP component
//! on the native Rust host. The backpressure axis: prove the ceiling (N allowed
//! then a 429), the cumulative quota decrementing, lockout after a burst, and a
//! verdict reaching a SEPARATE held-open SSE connection live.
//!
//! `CFG_MAX_ATTEMPTS=6` + `CFG_LOCKOUT_WINDOW=3` make the ceiling small and the
//! window short so the test is fast (defaults are 5 / 300s).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3030";

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
    let component = root.join("components/target/throttle_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-ratelimit`)");
    assert!(component.exists(), "composed wasm missing (just compose-ratelimit)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "throttle")
        .env("CFG_MAX_ATTEMPTS", "6")
        .env("CFG_LOCKOUT_WINDOW", "3")
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
    panic!("throttle host did not start");
}

#[test]
fn ceiling_quota_lockout_and_live_sse() {
    let _host = start_host();

    // ===== ceiling: with max-attempts=6, the 7th hit on a key is throttled ===
    let key = "kA";
    let mut allowed = 0;
    let mut first_429_at = None;
    for i in 1..=10 {
        let (code, _) = req("POST", "/api/hit", Some(json!({"key": key})));
        if code == 200 {
            allowed += 1;
        } else if first_429_at.is_none() {
            first_429_at = Some(i);
        }
    }
    assert!(allowed >= 1 && allowed <= 6, "should allow up to the ceiling (6), allowed {allowed}");
    assert!(first_429_at.is_some(), "a burst past the ceiling must produce a 429");

    // ===== quota decrements ==================================================
    let key2 = "kQ";
    let (_, s1) = req("GET", &format!("/api/state?key={key2}&subject={key2}"), None);
    let start_remaining = s1["quota_remaining"].as_u64().unwrap();
    req("POST", "/api/hit", Some(json!({"key": key2, "subject": key2})));
    let (_, s2) = req("GET", &format!("/api/state?key={key2}&subject={key2}"), None);
    let after_remaining = s2["quota_remaining"].as_u64().unwrap();
    assert!(after_remaining < start_remaining, "quota remaining should drop: {start_remaining} -> {after_remaining}");

    // ===== lockout via explicit failures + recovery ==========================
    // record-failure only strikes (it never returns Locked); lockout is OBSERVED
    // by check/state once the count reaches the ceiling. Strike past max=6.
    let key3 = "kL";
    for _ in 0..8 {
        req("POST", "/api/fail", Some(json!({"key": key3})));
    }
    let (_, st) = req("GET", &format!("/api/state?key={key3}"), None);
    assert_eq!(st["locked"], true, "enough failures should lock the key (observed via state): {st}");
    assert!(st["retry_after"].as_u64().unwrap() > 0, "a locked key reports retry-after");

    // recovery: after the 3s window, the key unlocks.
    std::thread::sleep(Duration::from_secs(4));
    let (_, st2) = req("GET", &format!("/api/state?key={key3}"), None);
    assert_eq!(st2["locked"], false, "the key should recover after the window: {st2}");

    // ===== LIVE SSE — a verdict reaches a separate held-open connection ======
    let found = Arc::new(AtomicBool::new(false));
    let f = found.clone();
    let url = format!("{}/api/stream", base());
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
                    if line.starts_with("data:") && line.contains("live-wall") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    });
    std::thread::sleep(Duration::from_millis(700));
    req("POST", "/api/hit", Some(json!({"key": "live-wall"})));
    reader.join().unwrap();
    assert!(found.load(Ordering::SeqCst), "the live SSE stream should carry the verdict");
}
