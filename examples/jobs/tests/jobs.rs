//! E2E for the jobs queue (JOBS.md) as ONE composed wasm HTTP component
//! (jobs-domain + outbox + inproc-workflow + cron + idempotency + records) on the
//! native Rust host. Proves the durable-job lifecycle over HTTP: a job runs to
//! `done`, a flaky job retries with backoff then succeeds, a `boom` job
//! dead-letters after the attempt cap and can be replayed, and an idempotency
//! key makes a repeated enqueue a no-op.
//!
//! Tuned via CFG: max-attempts=2 (dead on the 3rd failure), base-backoff=1s.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3038";

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

fn enqueue(body: Value) -> Value {
    let (s, v) = req("POST", "/api/jobs", Some(body));
    assert_eq!(s, 201, "enqueue: {v}");
    v["job"].clone()
}

fn tick() {
    let (s, _) = req("POST", "/api/tick", None);
    assert_eq!(s, 200);
}

/// The job with `id` from the board, if present.
fn job(id: &str) -> Option<Value> {
    let (_, b) = req("GET", "/api/jobs", None);
    b["jobs"].as_array()?.iter().find(|j| j["id"] == id).cloned()
}

fn state(id: &str) -> String {
    job(id).map(|j| j["state"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/jobs_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-jobs`)");
    assert!(component.exists(), "composed wasm missing (just compose-jobs)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "jobs")
        .env("CFG_MAX_ATTEMPTS", "2")
        .env("CFG_BASE_BACKOFF", "1")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        sleep(Duration::from_millis(50));
    }
    panic!("jobs host did not start");
}

#[test]
fn durable_job_lifecycle() {
    let _host = start_host();

    // ===== a plain job runs to done ==========================================
    let ok = enqueue(json!({"type": "email"}));
    let ok_id = ok["id"].as_str().unwrap().to_string();
    assert_eq!(ok["state"], "queued");
    tick();
    assert_eq!(state(&ok_id), "done", "email job completed");

    // ===== a flaky job fails, retries with backoff, then succeeds ============
    // fail_until=3 -> fails attempts 1 & 2, succeeds on attempt 3.
    let flaky = enqueue(json!({"type": "flaky", "payload": {"fail_until": 3}}));
    let flaky_id = flaky["id"].as_str().unwrap().to_string();
    tick(); // attempt 1 -> fail, backoff 1s
    assert_eq!(state(&flaky_id), "queued", "flaky requeued after first failure");
    sleep(Duration::from_millis(1100));
    tick(); // attempt 2 -> fail, backoff 2s
    assert_eq!(state(&flaky_id), "queued", "flaky requeued after second failure");
    sleep(Duration::from_millis(2100));
    tick(); // attempt 3 -> success
    let f = job(&flaky_id).unwrap();
    assert_eq!(f["state"], "done", "flaky eventually succeeds: {f}");
    assert!(f["attempts"].as_u64().unwrap() >= 3, "attempts counted: {f}");

    // ===== a boom job dead-letters after the attempt cap, then replays =======
    let boom = enqueue(json!({"type": "boom"}));
    let boom_id = boom["id"].as_str().unwrap().to_string();
    tick(); // fail 1
    sleep(Duration::from_millis(1100));
    tick(); // fail 2
    sleep(Duration::from_millis(2100));
    tick(); // fail 3 -> attempts(3) > max(2) -> dead
    let b = job(&boom_id).unwrap();
    assert_eq!(b["state"], "dead", "boom dead-lettered: {b}");
    assert!(b["error"].as_str().unwrap().contains("permanent"), "carries the failure: {b}");

    // replay moves it back to queued
    let (s, _) = req("POST", &format!("/api/jobs/{boom_id}/replay"), None);
    assert_eq!(s, 200);
    assert_eq!(state(&boom_id), "queued", "replayed dead job is queued again");

    // ===== exactly-once enqueue: a repeated key does not add a second job =====
    let a = enqueue(json!({"type": "email", "key": "welcome-42"}));
    let b = enqueue(json!({"type": "email", "key": "welcome-42"})); // duplicate
    assert_eq!(a["id"], b["id"], "same key replays the first job, no duplicate");
    let (_, board) = req("GET", "/api/jobs", None);
    let with_key = board["jobs"].as_array().unwrap().iter().filter(|j| j["id"] == a["id"]).count();
    assert_eq!(with_key, 1, "exactly one job for the idempotency key");
}
