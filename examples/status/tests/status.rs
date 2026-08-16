//! E2E for the status page (docs/apps/STATUS.md) as ONE composed wasm HTTP component on
//! the native Rust host. The timer-driven axis: the workload originates from
//! sched:timer, not an inbound request. A monitor is a recurring timer job;
//! `POST /api/tick` claims due jobs, probes each target over outgoing HTTP, and
//! drives an fsm:workflow instance per monitor (up -> degraded -> down needs TWO
//! consecutive failures; one good probe recovers).
//!
//! Note: monitors have a 10s minimum period, so this test sleeps across two
//! periods to prove the degraded -> down transition. It is deliberately slow.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3033";

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
    let component = root.join("components/target/status_page.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-status`)");
    assert!(component.exists(), "composed wasm missing (just compose-status)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "status")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        sleep(Duration::from_millis(50));
    }
    panic!("status host did not start");
}

fn id_of(status: &Value, name: &str) -> String {
    status["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("monitor {name} not found in {status}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn probe_transitions_up_degraded_down() {
    let host = start_host();

    // a monitor that probes the status page's OWN root (always 200 -> up) ...
    let (s, _) = req("POST", "/api/monitors", Some(json!({"name": "self", "url": base() + "/", "period": 10})));
    assert_eq!(s, 201, "create self monitor");
    // ... and one pointing at a dead port (connection refused -> failing).
    let (s, _) = req("POST", "/api/monitors", Some(json!({"name": "dead", "url": "http://127.0.0.1:59999/", "period": 10})));
    assert_eq!(s, 201, "create dead monitor");

    // tick 1: both are due. self probes 200 (stays up); dead fails once
    // (up -> degraded — one failure is not yet down).
    let (_, t1) = req("POST", "/api/tick", None);
    assert_eq!(t1["due"].as_u64().unwrap(), 2, "both monitors due: {t1}");
    let results = t1["results"].as_array().unwrap();
    let self_r = results.iter().find(|r| r["status"] == 200).expect("self probe 200");
    assert!(self_r["ok"].as_bool().unwrap(), "self probe ok");
    let dead_r = results.iter().find(|r| r["ok"] == false).expect("dead probe fails");
    assert_eq!(dead_r["transition"], "up->degraded", "one failure degrades, not down: {dead_r}");

    // wait out the period, tick 2: dead fails a SECOND consecutive time
    // (degraded -> down).
    sleep(Duration::from_secs(11));
    let (_, t2) = req("POST", "/api/tick", None);
    let dead_r = t2["results"].as_array().unwrap().iter().find(|r| r["ok"] == false).expect("dead still failing");
    assert_eq!(dead_r["transition"], "degraded->down", "second failure takes it down: {dead_r}");

    // status reflects self=up, dead=down.
    let (_, st) = req("GET", "/api/status", None);
    let mons = st["monitors"].as_array().unwrap();
    assert_eq!(mons.iter().find(|m| m["name"] == "self").unwrap()["state"], "up");
    assert_eq!(mons.iter().find(|m| m["name"] == "dead").unwrap()["state"], "down");

    // the fsm transition log records both hops for the dead monitor.
    let dead_id = id_of(&st, "dead");
    let (_, hist) = req("GET", &format!("/api/monitors/{dead_id}/history"), None);
    let h = hist["history"].as_array().unwrap();
    assert_eq!(h.len(), 2, "two transitions logged: {hist}");
    assert_eq!(h[0]["to"], "degraded");
    assert_eq!(h[1]["to"], "down");

    drop(host);
}
