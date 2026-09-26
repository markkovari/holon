//! A durable record of an outstanding call, end to end (ADR-0100): every
//! request goes
//!
//!   this test → comp-host (park-gateway ⊕ park-store) → comp-park → NATS JetStream
//!
//! over real HTTP, through the component model, against a real NATS — never
//! the library. `bash e2e/park.sh` brings NATS up (a private `nats-server -js`
//! on a free port), builds what this needs, runs this file and tears it down.
//!
//! Without `PARK_E2E_NATS_URL` every test here prints `SKIPPED` and returns:
//! CI compiles this file (`--no-run`) and does not run it, because a skip that
//! returns `ok` is a green tick for a test that did not run.
//!
//! Each test owns its bucket and wake stream (`--bucket`/`--wake-stream`, named
//! after the test), its `comp-park` port and its `comp-host`, so they run in
//! parallel without seeing each other's tickets.

mod gatelib;

use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn nats_url(test: &str) -> Option<String> {
    match std::env::var("PARK_E2E_NATS_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            eprintln!(
                "SKIPPED e2e_park::{test}: set PARK_E2E_NATS_URL (bash e2e/park.sh does) — \
                 this needs a real NATS JetStream, and it did NOT run"
            );
            None
        }
    }
}

/// A running `comp-park`, and the `park-gateway ⊕ park-store` component in
/// front of it.
struct Stack {
    name: String,
    park: Child,
    logs: tempfile::TempDir,
    gate: gatelib::Gate,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.park.kill();
        let _ = self.park.wait();
    }
}

impl Stack {
    fn up(test: &str, nats: &str) -> Option<Stack> {
        let tag = format!("{test}-{}-{}", std::process::id(), nanos() % 1_000_000_000);
        let port = free_port();
        let logs = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(logs.path().join("park.log")).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comp-park"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .args(["--nats-url", nats])
            .args(["--bucket", &format!("e2e-park-{tag}")])
            .args(["--wake-stream", &format!("E2E_PARK_WAKE_{}", tag.replace(['-', '.'], "_"))])
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = cmd.spawn().expect("spawn comp-park");
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();

        let t0 = Instant::now();
        loop {
            if let Ok(r) = http.get(format!("http://127.0.0.1:{port}/health")).send() {
                let v: Value = r.json().unwrap_or(Value::Null);
                if v["ok"] == true {
                    break;
                }
            }
            if let Ok(Some(st)) = child.try_wait() {
                panic!(
                    "[{test}] comp-park exited during startup: {st}\n{}",
                    std::fs::read_to_string(logs.path().join("park.log")).unwrap_or_default()
                );
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "[{test}] comp-park never became healthy"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let url = format!("park-url=http://127.0.0.1:{port}");
        let egress = format!("127.0.0.1:{port}");
        let gate = gatelib::Gate::compose_and_start_with_egress(
            "park",
            "park-gateway",
            &[&url],
            &[&egress],
        )?;
        Some(Stack { name: test.to_string(), park: child, logs, gate })
    }

    fn call(&self, func: &str, body: Value) -> (u16, Value) {
        let (status, text) = self.gate.post(&format!("/v1/{func}"), None, body);
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("[{}] {func}: {status} with a body that is not JSON ({e}): {text}", self.name)
        });
        (status, v)
    }

    fn ok(&self, func: &str, body: Value) -> Value {
        let (status, v) = self.call(func, body);
        assert_eq!(status, 200, "[{}] {func} refused: {v}", self.name);
        v
    }

    /// The park daemon's own log, for a panic message.
    fn park_log(&self) -> String {
        std::fs::read_to_string(self.logs.path().join("park.log")).unwrap_or_default()
    }
}

fn agent(id: &str) -> Value {
    json!({ "id": id, "goal": "e2e", "model": null })
}

fn call_of(correlation: &str) -> Value {
    json!({ "correlation": correlation, "description": "e2e call", "deadline": null, "poll": null })
}

#[test]
fn park_wake_take_ready_over_the_real_component_chain() {
    let Some(nats) = nats_url("park_wake_take_ready_over_the_real_component_chain") else { return };
    let Some(s) = Stack::up("park_wake_take_ready_over_the_real_component_chain", &nats) else {
        return;
    };

    let session = format!("s-{}", nanos());
    let parked =
        s.ok("park", json!({ "session": session, "call": call_of("req-1"), "by": agent("a1") }));
    assert_eq!(parked["outcome"], "parked", "{}", s.park_log());
    let ticket = parked["ticket"].as_str().unwrap().to_string();

    // Not ready yet: `take-ready` is `not-found`, not a 200.
    let (status, _) = s.call("take-ready", json!({ "ticket": ticket }));
    assert_eq!(status, 404);

    let (status, woken) = s.call(
        "wake",
        json!({ "correlation": "req-1", "answer": { "ok": true, "body": [42], "detail": null } }),
    );
    assert_eq!(status, 200, "{woken}");
    assert_eq!(woken.as_str(), Some(ticket.as_str()));

    let pending = s.ok("pending", json!({ "session": session }));
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["status"], "ready");

    let result = s.ok("take-ready", json!({ "ticket": ticket }));
    assert_eq!(result["ok"], true);
    assert_eq!(result["body"], json!([42]));

    // Exactly once: a second `take-ready` is `already-closed`.
    let (status, refused) = s.call("take-ready", json!({ "ticket": ticket }));
    assert_eq!((status, refused["error"].as_str()), (409, Some("already-closed")));

    let log = s.ok("oplog", json!({ "session": session, "after": null, "limit": 10 }));
    assert_eq!(log.as_array().unwrap().len(), 1);
    assert_eq!(log[0]["status"], "resumed");
}

#[test]
fn reparking_and_cancelling_over_the_real_component_chain() {
    let Some(nats) = nats_url("reparking_and_cancelling_over_the_real_component_chain") else {
        return;
    };
    let Some(s) = Stack::up("reparking_and_cancelling_over_the_real_component_chain", &nats) else {
        return;
    };

    let session = format!("s-{}", nanos());
    let first =
        s.ok("park", json!({ "session": session, "call": call_of("req-1"), "by": agent("a1") }));
    let second =
        s.ok("park", json!({ "session": session, "call": call_of("req-1"), "by": agent("a1") }));
    assert_eq!(first["ticket"], second["ticket"]);
    assert_eq!(second["outcome"], "already-parked");

    let ticket = first["ticket"].as_str().unwrap().to_string();
    s.ok("cancel", json!({ "ticket": ticket, "by": agent("a1") }));

    let pending = s.ok("pending", json!({ "session": session }));
    assert!(
        pending.as_array().unwrap().is_empty(),
        "a cancelled ticket must not show up as pending: {pending}"
    );

    // A late wake against a cancelled ticket is accepted, not an error.
    let (status, _) = s.call(
        "wake",
        json!({ "correlation": "req-1", "answer": { "ok": true, "body": [], "detail": null } }),
    );
    assert_eq!(status, 200);

    let (status, refused) = s.call("take-ready", json!({ "ticket": ticket }));
    assert_eq!((status, refused["error"].as_str()), (409, Some("already-closed")));
}
