//! E2E for the pipeline board (PIPELINE.md) as ONE composed wasm HTTP component
//! on the native Rust host.
//!
//! Rung 1: enqueue + snapshot. Rung 2 (the headline): an event enqueued by one
//! request is dispatched at-least-once and its transitions arrive LIVE over a
//! separate, held-open Server-Sent-Events connection. Rung 3 (the axis no other
//! showcase shows): with the downstream sink taken DOWN, an event retries and
//! then drops to the dead-letter tray — and a Replay requeues it.
//!
//! `CFG_MAX_ATTEMPTS=1` + `CFG_BASE_BACKOFF=1` make the retry ceiling reachable
//! in a couple of seconds so the DLQ path is testable (defaults are 5 / 5s).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3026";

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
    let component = root.join("components/target/pipeline_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-pipeline`)");
    assert!(component.exists(), "composed wasm missing (just compose-pipeline)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "pipeline")
        // a low retry ceiling + short backoff so the dead-letter path is fast.
        .env("CFG_MAX_ATTEMPTS", "1")
        .env("CFG_BASE_BACKOFF", "1")
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
    panic!("pipeline host did not start");
}

/// Find a transition for `id` reaching `state` in the snapshot, retrying until
/// `deadline` (a plain GET pumps the relay, so polling also advances it).
fn wait_state(id: &str, state: &str, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        let (_, snap) = req("GET", "/api/events?after=-1", None);
        let hit = snap["transitions"]
            .as_array()
            .map(|rows| {
                rows.iter().any(|r| r["id"] == json!(id) && r["state"] == json!(state))
            })
            .unwrap_or(false);
        if hit {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[test]
fn enqueue_dispatch_deadletter_replay() {
    let _host = start_host();

    // ===== rung 1: enqueue + snapshot =======================================
    let (status, e) = req("POST", "/api/events", Some(json!({"topic": "invoice.paid", "payload": {"amount": 100}})));
    assert_eq!(status, 201, "enqueue: {e}");
    assert_eq!(e["state"], "pending");
    let id = e["id"].as_str().unwrap().to_string();

    // the snapshot pumps the relay; with the sink up (default), it dispatches.
    let acked = wait_state(&id, "acked", Instant::now() + Duration::from_secs(6));
    assert!(acked, "event should dispatch and be acked with the sink up");

    // ===== rung 2: LIVE SSE — the headline ==================================
    // A reader holds GET /api/stream open; an event enqueued by a separate
    // request must reach it as a data: frame (marching to acked).
    let found = Arc::new(AtomicBool::new(false));
    let f = found.clone();
    let url = format!("{}/api/stream", base());
    let reader = std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout_read(Duration::from_secs(2)).build();
        let Ok(resp) = agent.get(&url).call() else { return };
        let mut buf = BufReader::new(resp.into_reader());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            match buf.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.starts_with("data:") && line.contains("live.evt") && line.contains("acked") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {} // read-timeout tick — heartbeats keep us moving
            }
        }
    });
    std::thread::sleep(Duration::from_millis(700)); // let the reader pin its cursor
    let (status, _) = req("POST", "/api/events", Some(json!({"topic": "live.evt", "payload": {"n": 1}})));
    assert_eq!(status, 201);
    reader.join().unwrap();
    assert!(found.load(Ordering::SeqCst), "the live SSE stream should carry the acked transition");

    // ===== rung 3: sink down → retry → dead-letter → replay =================
    let (status, s) = req("POST", "/api/sink", Some(json!({"up": false})));
    assert_eq!(status, 200, "sink down: {s}");
    assert_eq!(s["sink_up"], false);

    let (status, e) = req("POST", "/api/events", Some(json!({"topic": "flaky.evt", "payload": {"n": 2}})));
    assert_eq!(status, 201, "enqueue while down: {e}");
    let dead_id = e["id"].as_str().unwrap().to_string();

    // with max-attempts=1, it fails once (retry) then dead-letters. Snapshots
    // pump the relay; backoff is 1s so a few polls exhaust the ceiling.
    let dead = wait_state(&dead_id, "dead", Instant::now() + Duration::from_secs(12));
    assert!(dead, "an event should dead-letter when the sink stays down");

    // the dead-letter tray lists it.
    let (_, dl) = req("GET", "/api/dead-letters", None);
    let listed = dl["dead"].as_array().unwrap().iter().any(|d| d["id"] == json!(dead_id));
    assert!(listed, "dead event should appear in the dead-letter tray: {dl}");

    // bring the sink back up, then Replay: it requeues and now delivers.
    req("POST", "/api/sink", Some(json!({"up": true})));
    let (status, r) = req("POST", &format!("/api/dead-letters/{dead_id}/replay"), None);
    assert_eq!(status, 200, "replay: {r}");
    let redelivered = wait_state(&dead_id, "acked", Instant::now() + Duration::from_secs(8));
    assert!(redelivered, "a replayed event should dispatch and be acked once the sink is back up");
}
