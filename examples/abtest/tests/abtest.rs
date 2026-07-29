//! E2E for the A/B/n experiment console (EXPERIMENT.md) as ONE composed wasm
//! HTTP component on the native Rust host — two new contracts
//! (experiment:assign + metrics:collect) exercised through the domain.
//!
//! Proves: (1) assignment is STICKY per subject; (2) two DIFFERENT subjects can
//! land in DIFFERENT arms (the diff-vs-rollout point — a boolean flag can't);
//! (3) a 50/25/25 weight config splits a cohort ~50/25/25; (4) raising a weight
//! is monotone (an arm only gains subjects); (5) conversions attribute to the
//! assigned arm's rate; (6) an outcome recorded by one request reaches a
//! SEPARATE held-open SSE connection live.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3028";

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
    let component = root.join("components/target/abtest_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-abtest`)");
    assert!(component.exists(), "composed wasm missing (just compose-abtest)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "abtest")
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
    panic!("abtest host did not start");
}

/// Arm counts across the cohort grid.
fn arm_counts(n: u32) -> std::collections::HashMap<String, u32> {
    let (_, c) = req("GET", &format!("/api/cohort?exp=checkout&n={n}"), None);
    let mut m = std::collections::HashMap::new();
    for cell in c["cells"].as_array().unwrap() {
        *m.entry(cell["arm"].as_str().unwrap().to_string()).or_insert(0) += 1;
    }
    m
}

/// The set of subjects currently in `arm`.
fn subjects_in(arm: &str, n: u32) -> std::collections::HashSet<String> {
    let (_, c) = req("GET", &format!("/api/cohort?exp=checkout&n={n}"), None);
    c["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["arm"].as_str() == Some(arm))
        .map(|cell| cell["subject"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn assign_sticky_split_attribute_and_live_sse() {
    let _host = start_host();

    // ===== define a 50/25/25 experiment ======================================
    let (status, _) = req(
        "POST",
        "/api/experiments/checkout",
        Some(json!({"tenant": "", "variants": [
            {"name": "control", "weight": 50},
            {"name": "variant-a", "weight": 25},
            {"name": "variant-b", "weight": 25}
        ]})),
    );
    assert_eq!(status, 200);

    // (1) sticky: the same subject assigns to the same arm across calls.
    let (_, a1) = req("GET", "/api/assign?exp=checkout&subject=user-42", None);
    let (_, a2) = req("GET", "/api/assign?exp=checkout&subject=user-42", None);
    assert_eq!(a1["arm"], a2["arm"], "a subject must not switch arms between calls");

    // (2) two DIFFERENT subjects can land in different arms (a flag can't do this).
    let arms_seen: std::collections::HashSet<String> = (0..100)
        .map(|i| {
            let (_, a) = req("GET", &format!("/api/assign?exp=checkout&subject=u{i}"), None);
            a["arm"].as_str().unwrap().to_string()
        })
        .collect();
    assert!(arms_seen.len() >= 2, "different subjects should spread across arms, saw {arms_seen:?}");

    // (3) ~50/25/25 split across a 500-subject cohort (wide bands — it's a hash).
    let counts = arm_counts(500);
    let control = *counts.get("control").unwrap_or(&0);
    let a = *counts.get("variant-a").unwrap_or(&0);
    let b = *counts.get("variant-b").unwrap_or(&0);
    assert!((200..=300).contains(&control), "control ≈250, got {control}");
    assert!((100..=175).contains(&a), "variant-a ≈125, got {a}");
    assert!((100..=175).contains(&b), "variant-b ≈125, got {b}");

    // (4) monotone: raising control to 80 only ADDS to control (no arm-hopping
    // for subjects already in control).
    let control_before = subjects_in("control", 500);
    req(
        "POST",
        "/api/experiments/checkout",
        Some(json!({"tenant": "", "variants": [
            {"name": "control", "weight": 80},
            {"name": "variant-a", "weight": 10},
            {"name": "variant-b", "weight": 10}
        ]})),
    );
    let control_after = subjects_in("control", 500);
    assert!(control_after.len() > control_before.len(), "control should grow: {} -> {}", control_before.len(), control_after.len());
    assert!(control_before.is_subset(&control_after), "no subject already in control may leave it when its weight rises");

    // restore the even-ish split for the attribution check.
    req(
        "POST",
        "/api/experiments/checkout",
        Some(json!({"tenant": "", "variants": [
            {"name": "control", "weight": 50},
            {"name": "variant-a", "weight": 50}
        ]})),
    );

    // (5) attribution: expose two subjects known to be in different arms,
    // convert one, and check the rate lands on the right arm.
    // Find one subject per arm.
    let mut in_control = None;
    let mut in_a = None;
    for i in 0..200 {
        let s = format!("cv{i}");
        let (_, a) = req("GET", &format!("/api/assign?exp=checkout&subject={s}"), None);
        match a["arm"].as_str().unwrap() {
            "control" if in_control.is_none() => in_control = Some(s),
            "variant-a" if in_a.is_none() => in_a = Some(s),
            _ => {}
        }
        if in_control.is_some() && in_a.is_some() {
            break;
        }
    }
    let sc = in_control.expect("a control subject");
    let sa = in_a.expect("a variant-a subject");
    // expose both, convert only the control one.
    for s in [&sc, &sa] {
        req("POST", "/api/expose", Some(json!({"exp": "checkout", "subject": s})));
    }
    req("POST", "/api/convert", Some(json!({"exp": "checkout", "subject": sc})));

    let (_, res) = req("GET", "/api/results?exp=checkout", None);
    let arms = res["arms"].as_array().unwrap();
    let control_arm = arms.iter().find(|x| x["name"] == "control").unwrap();
    let a_arm = arms.iter().find(|x| x["name"] == "variant-a").unwrap();
    assert_eq!(control_arm["converted"].as_u64().unwrap(), 1, "control got the conversion");
    assert_eq!(a_arm["converted"].as_u64().unwrap(), 0, "variant-a got no conversion");
    assert!((control_arm["rate"].as_f64().unwrap() - 1.0).abs() < 1e-9, "control rate 1/1 = 1.0");

    // ===== (6) LIVE SSE — the headline ======================================
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
                    if line.starts_with("data:") && line.contains("live-subject") && line.contains("converted") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    });
    std::thread::sleep(Duration::from_millis(700));
    let (status, _) = req("POST", "/api/convert", Some(json!({"exp": "checkout", "subject": "live-subject"})));
    assert_eq!(status, 200);
    reader.join().unwrap();
    assert!(found.load(Ordering::SeqCst), "the live SSE stream should carry the conversion event");
}
