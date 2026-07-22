//! E2E for the rollout console (FLAGS.md) as ONE composed wasm HTTP component on
//! the native Rust host.
//!
//! Rung 1: set + evaluate. The axis the console makes visible — STICKINESS: a
//! percentage rollout buckets on a stable hash, so (a) a given subject lands on
//! the same side across repeated evals, and (b) raising the percentage only ever
//! ADDS subjects — it never turns an already-on subject off (a monotone cohort).
//! Rung 2 (the headline): a rule flip made by one request reaches a SEPARATE
//! held-open SSE connection live.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3027";

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
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/flags_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-flags`)");
    assert!(component.exists(), "composed wasm missing (just compose-flags)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "flags")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("flags host did not start");
}

/// The set of subjects currently ON for `flag`, from the cohort grid.
fn on_set(flag: &str, n: u32) -> std::collections::HashSet<String> {
    let (_, c) = req("GET", &format!("/api/cohort?flag={flag}&n={n}"), None);
    c["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["enabled"].as_bool().unwrap_or(false))
        .map(|cell| cell["subject"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn set_evaluate_sticky_and_live_sse() {
    let _host = start_host();

    // ===== rung 1: set + sticky cohorts =====================================
    let (status, _) = req("POST", "/api/flags/new-checkout", Some(json!({"tenant": "", "rule": 30})));
    assert_eq!(status, 200);

    // ~30% of 100 subjects on (stable hash — allow a wide band, it's a hash).
    let at30 = on_set("new-checkout", 100);
    assert!((15..=45).contains(&at30.len()), "≈30% expected, got {}", at30.len());

    // (a) sticky: the same subject lands the same way across repeated evals.
    let sample = "subject-7";
    let (_, e1) = req("GET", &format!("/api/eval?flag=new-checkout&subject={sample}"), None);
    let (_, e2) = req("GET", &format!("/api/eval?flag=new-checkout&subject={sample}"), None);
    assert_eq!(e1["enabled"], e2["enabled"], "a subject must not flicker between evals");

    // (b) monotone: raising to 60% keeps every already-on subject on.
    req("POST", "/api/flags/new-checkout", Some(json!({"tenant": "", "rule": 60})));
    let at60 = on_set("new-checkout", 100);
    assert!(at60.len() > at30.len(), "raising the % should add subjects: {} -> {}", at30.len(), at60.len());
    assert!(at30.is_subset(&at60), "no already-on subject may turn off when the % rises");

    // kill-switch: off wins over any percentage — all dark.
    req("POST", "/api/flags/new-checkout", Some(json!({"tenant": "", "rule": "off"})));
    assert_eq!(on_set("new-checkout", 100).len(), 0, "kill-switch must darken every subject");

    // ===== rung 2: LIVE SSE — the headline ==================================
    // A reader holds GET /api/stream open; a rule flip by a separate request
    // must reach it as a data: frame.
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
                    if line.starts_with("data:") && line.contains("dark-mode") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    });
    std::thread::sleep(Duration::from_millis(700)); // let the reader pin its cursor
    let (status, _) = req("POST", "/api/flags/dark-mode", Some(json!({"tenant": "", "rule": "on"})));
    assert_eq!(status, 200);
    reader.join().unwrap();
    assert!(found.load(Ordering::SeqCst), "the live SSE stream should carry the rule change");
}
