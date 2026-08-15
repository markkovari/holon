//! E2E for the trip-booking saga (docs/apps/SAGA.md) as ONE composed wasm HTTP component
//! on the native Rust host. Every route is the Rust saga-domain orchestrating
//! fsm-workflow + record-store + idempotency-guard + event-bus, linked into one
//! .wasm. Covers rung 1 (commit) and rung 2 (compensation) — same engine, two
//! outcomes.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3022";

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
        Err(e) => panic!("{method} {path}: transport error: {e}"),
    };
    let status = resp.status();
    let text = resp.into_string().unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/saga_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-saga`)");
    assert!(component.exists(), "composed wasm missing: {component:?} (just compose-saga)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "saga")
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
    panic!("saga host did not start at {}", base());
}

fn steps(saga: &Value) -> Vec<(String, String, String)> {
    saga["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["leg"].as_str().unwrap_or("").to_string(),
                s["state"].as_str().unwrap_or("").to_string(),
                s["ref"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn state_of(saga: &Value, leg: &str) -> String {
    steps(saga).into_iter().find(|(l, _, _)| l == leg).map(|(_, s, _)| s).unwrap_or_default()
}

#[test]
fn saga_commit_and_compensate() {
    let _host = start_host();

    // ===== rung 1: happy path — all three legs book, saga commits ============
    let (status, saga) = req("POST", "/trips", Some(json!({"traveler": "Ada Lovelace"})));
    assert_eq!(status, 201, "create: {saga}");
    let id = saga["id"].as_str().expect("id").to_string();
    assert_eq!(saga["status"], "running");
    assert_eq!(steps(&saga).iter().filter(|(_, s, _)| s == "pending").count(), 3);

    let (status, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(status, 200, "run: {saga}");
    assert_eq!(saga["status"], "committed", "should commit: {saga}");
    for (leg, st, r) in steps(&saga) {
        assert_eq!(st, "booked", "{leg} should be booked");
        assert!(!r.is_empty(), "{leg} should have a booking ref");
    }
    let flight_ref = steps(&saga).into_iter().find(|(l, _, _)| l == "flight").unwrap().2;
    assert!(flight_ref.starts_with("FL-"), "flight ref: {flight_ref}");

    // fsm history records the commit
    let (_, saga) = req("GET", &format!("/trips/{id}"), None);
    assert!(
        saga["history"].as_array().unwrap().iter().any(|h| h["event"] == "commit"),
        "history should contain commit: {saga}"
    );

    // idempotent: re-running a committed saga changes nothing (no double-book)
    let (_, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(saga["status"], "committed");
    assert_eq!(
        steps(&saga).into_iter().find(|(l, _, _)| l == "flight").unwrap().2,
        flight_ref,
        "booking ref must be stable across re-runs"
    );

    // ===== rung 2: compensation — a mid-saga failure rolls back ==============
    // car fails: flight + hotel book, then compensate in reverse → compensated.
    let (_, saga) = req("POST", "/trips", Some(json!({"traveler": "Grace", "failLeg": "car"})));
    let id = saga["id"].as_str().unwrap().to_string();
    let (status, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(status, 200);
    assert_eq!(saga["status"], "compensated", "car failure should compensate: {saga}");
    assert_eq!(state_of(&saga, "flight"), "compensated", "flight rolled back");
    assert_eq!(state_of(&saga, "hotel"), "compensated", "hotel rolled back");
    assert_eq!(state_of(&saga, "car"), "failed", "car failed");
    let hist = saga["history"].as_array().unwrap();
    assert!(hist.iter().any(|h| h["event"] == "fail"), "history has fail: {saga}");
    assert!(hist.iter().any(|h| h["event"] == "compensated"), "history has compensated");

    // first leg fails: nothing booked, so nothing to compensate → compensated.
    let (_, saga) = req("POST", "/trips", Some(json!({"traveler": "Alan", "failLeg": "flight"})));
    let id = saga["id"].as_str().unwrap().to_string();
    let (_, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(saga["status"], "compensated", "first-leg failure: {saga}");
    assert_eq!(state_of(&saga, "flight"), "failed");
    assert_eq!(state_of(&saga, "hotel"), "pending", "never reached");
    assert_eq!(state_of(&saga, "car"), "pending", "never reached");

    // idempotent re-run of a terminal (compensated) saga is a no-op
    let (_, saga2) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(saga2["status"], "compensated");

    // ===== rung 3: retries + resumable advance ===============================

    // a flaky leg that fails transiently then recovers → still commits.
    let (_, saga) = req("POST", "/trips", Some(json!({"traveler": "Katherine", "flakyLeg": "hotel", "flakyFails": 2})));
    let id = saga["id"].as_str().unwrap().to_string();
    let (_, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(saga["status"], "committed", "flaky-but-recovering should commit: {saga}");
    assert_eq!(state_of(&saga, "hotel"), "booked");
    let hotel = saga["steps"].as_array().unwrap().iter().find(|s| s["leg"] == "hotel").unwrap();
    assert_eq!(hotel["attempts"].as_u64().unwrap(), 2, "hotel should have retried twice: {saga}");

    // a flaky leg that never recovers → give up after the retry ceiling → compensate.
    let (_, saga) = req("POST", "/trips", Some(json!({"traveler": "Dorothy", "flakyLeg": "hotel", "flakyFails": 9})));
    let id = saga["id"].as_str().unwrap().to_string();
    let (_, saga) = req("POST", &format!("/trips/{id}/run"), None);
    assert_eq!(saga["status"], "compensated", "exhausted retries should compensate: {saga}");
    assert_eq!(state_of(&saga, "flight"), "compensated");
    assert_eq!(state_of(&saga, "hotel"), "failed");

    // resumable advance: `pump` moves the saga exactly ONE persisted step at a
    // time — the same mechanism that resumes a saga after a host restart.
    let (_, saga) = req("POST", "/trips", Some(json!({"traveler": "Margaret"})));
    let id = saga["id"].as_str().unwrap().to_string();
    let pump_get = |id: &str| -> Value {
        req("POST", "/internal/pump", None);
        req("GET", &format!("/trips/{id}"), None).1
    };
    let s = pump_get(&id); // 1: flight
    assert_eq!(s["status"], "running");
    assert_eq!(state_of(&s, "flight"), "booked");
    assert_eq!(state_of(&s, "hotel"), "pending");
    let s = pump_get(&id); // 2: hotel
    assert_eq!(state_of(&s, "hotel"), "booked");
    assert_eq!(state_of(&s, "car"), "pending");
    let s = pump_get(&id); // 3: car
    assert_eq!(state_of(&s, "car"), "booked");
    assert_eq!(s["status"], "running", "not committed until the finalize step");
    let s = pump_get(&id); // 4: commit
    assert_eq!(s["status"], "committed", "final pump commits: {s}");
}
