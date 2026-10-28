//! A durable record of an outstanding call, end to end (ADR-0100): every request goes
//!
//!   this test -> comp-host (park-gateway + park-store) -> comp-park -> NATS JetStream
//!
//! over real HTTP, through the component model, against a real NATS, never the library. `bash e2e/park.sh`
//! brings NATS up, builds what is needed, runs this file and tears it down.
//!
//! Without `PARK_E2E_NATS_URL` every test prints `SKIPPED` and returns (CI compiles with `--no-run`, since a
//! skip that returns `ok` is a green tick for a test that did not run).
//!
//! Each test owns its bucket, wake stream, `comp-park` port and `comp-host`, so they run in parallel.
//!
//! The resumer tests (ADR-0106) add a real `comp-resume` process and a stub sink it delivers to, and
//! kill the resumer outright mid-delivery.

// The gate harness the root's suites share, included rather than copied (docs/adr/0115).
#[path = "../../../tests/gatelib/mod.rs"]
mod gatelib;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
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

/// A running `comp-park`, and the `park-gateway + park-store` component in front of it.
struct Stack {
    name: String,
    park: Child,
    logs: tempfile::TempDir,
    gate: gatelib::Gate,
    nats: String,
    bucket: String,
    wake_stream: String,
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
        let bucket = format!("e2e-park-{tag}");
        let wake_stream = format!("E2E_PARK_WAKE_{}", tag.replace(['-', '.'], "_"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_holon-park"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .args(["--nats-url", nats])
            .args(["--bucket", &bucket])
            .args(["--wake-stream", &wake_stream])
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
        Some(Stack {
            name: test.to_string(),
            park: child,
            logs,
            gate,
            nats: nats.to_string(),
            bucket,
            wake_stream,
        })
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
    json!({ "correlation": correlation, "description": "e2e call", "deadline": null, "poll": null,
            "replay": "unsafe" })
}

fn call_replaying(correlation: &str, replay: &str, deadline: Option<u64>) -> Value {
    json!({ "correlation": correlation, "description": format!("e2e {correlation}"),
            "deadline": deadline, "poll": null, "replay": replay })
}

fn now_ms() -> u64 {
    (nanos() / 1_000_000) as u64
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
    assert_eq!(result["answered"]["ok"], true, "{result}");
    assert_eq!(result["answered"]["body"], json!([42]));

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

// ---- ADR-0106: interrupted, replay, and the resumer ----------------------------

#[test]
fn an_interrupted_call_is_taken_as_an_interruption_through_the_component_chain() {
    let name = "an_interrupted_call_is_taken_as_an_interruption_through_the_component_chain";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };

    let session = format!("s-{}", nanos());
    let parked = s.ok(
        "park",
        json!({ "session": session, "call": call_replaying("pay-1", "unsafe", None), "by": agent("a1") }),
    );
    let ticket = parked["ticket"].as_str().unwrap().to_string();

    // An empty reason is refused: it is what the turn will be told.
    let (status, refused) =
        s.call("interrupt", json!({ "ticket": ticket, "reason": " ", "by": agent("a1") }));
    assert_eq!((status, refused["error"].as_str()), (400, Some("invalid")), "{refused}");

    let said = s.call(
        "interrupt",
        json!({ "ticket": ticket, "reason": "the box restarted", "by": agent("a1") }),
    );
    assert_eq!(said, (200, json!("interrupted")));
    // Idempotent, and the first reason stands.
    let again =
        s.call("interrupt", json!({ "ticket": ticket, "reason": "second", "by": agent("a1") }));
    assert_eq!(again, (200, json!("interrupted")));

    let pending = s.ok("pending", json!({ "session": session }));
    assert_eq!(pending[0]["status"], "interrupted");
    assert_eq!(pending[0]["correlation"], "pay-1");
    assert!(pending[0]["interrupted-at"].is_u64(), "{pending}");

    // A late answer is accepted and dropped.
    let (status, _) = s.call(
        "wake",
        json!({ "correlation": "pay-1", "answer": { "ok": true, "body": [], "detail": null } }),
    );
    assert_eq!(status, 200);

    let taken = s.ok("take-ready", json!({ "ticket": ticket }));
    assert_eq!(taken["interrupted"]["reason"], "the box restarted", "{taken}");
    assert_eq!(taken["interrupted"]["by"]["id"], "a1");
    assert!(taken.get("answered").is_none());

    let (status, refused) = s.call("take-ready", json!({ "ticket": ticket }));
    assert_eq!((status, refused["error"].as_str()), (409, Some("already-closed")));
    let (status, refused) =
        s.call("interrupt", json!({ "ticket": ticket, "reason": "why", "by": agent("a1") }));
    assert_eq!((status, refused["error"].as_str()), (409, Some("already-closed")));
}

#[test]
fn replay_and_the_entry_fields_survive_the_component_chain() {
    let name = "replay_and_the_entry_fields_survive_the_component_chain";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };

    let session = format!("s-{}", nanos());
    let deadline = now_ms() + 3_600_000;
    s.ok(
        "park",
        json!({ "session": session, "call": call_replaying("read-1", "safe", Some(deadline)),
                "by": agent("a1") }),
    );
    // A caller that leaves `replay` out gets the cautious reading.
    s.ok(
        "park",
        json!({ "session": session, "by": agent("a1"),
                "call": { "correlation": "old-1", "description": "d", "deadline": null, "poll": null } }),
    );

    let log = s.ok("oplog", json!({ "session": session, "after": null, "limit": 10 }));
    let by_corr =
        |c: &str| log.as_array().unwrap().iter().find(|e| e["correlation"] == c).unwrap().clone();
    let safe = by_corr("read-1");
    assert_eq!(safe["replay"], "safe");
    assert_eq!(safe["deadline"], json!(deadline));
    assert_eq!(safe["redispatches"], 0);
    assert_eq!(by_corr("old-1")["replay"], "unsafe");
}

/// A sink the resumer can deliver to: records what it is handed, and answers
/// according to its current mode.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Accept,
    Hang,
}

struct StubSink {
    addr: SocketAddr,
    mode: Arc<Mutex<Mode>>,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl StubSink {
    fn start() -> StubSink {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mode = Arc::new(Mutex::new(Mode::Accept));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (m, sn) = (mode.clone(), seen.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (m, sn) = (m.clone(), sn.clone());
                std::thread::spawn(move || serve_one(conn, m, sn));
            }
        });
        StubSink { addr, mode, seen }
    }

    fn url(&self) -> String {
        format!("http://{}/deliver", self.addr)
    }

    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().unwrap().clone()
    }
}

/// One HTTP request, by hand: headers, a `Content-Length` body, a one-line answer.
fn serve_one(mut conn: TcpStream, mode: Arc<Mutex<Mode>>, seen: Arc<Mutex<Vec<(String, Value)>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, len) = loop {
        let n = conn.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0usize);
            break (i + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let n = conn.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let key = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("idempotency-key").then(|| v.trim().to_string())
        })
        .unwrap_or_default();
    let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
    seen.lock().unwrap().push((key, body));

    if *mode.lock().unwrap() == Mode::Hang {
        std::thread::sleep(Duration::from_secs(3600));
        return;
    }
    let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Resumer {
    child: Child,
}

impl Drop for Resumer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Stack {
    /// A real `comp-resume` against this stack's NATS, bucket and stream.
    fn resumer(&self, sink_url: &str) -> Resumer {
        let log = std::fs::File::create(self.logs.path().join("resume.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_holon-resume"))
            .args(["--sink-url", sink_url])
            .args(["--nats-url", &self.nats])
            .args(["--bucket", &self.bucket])
            .args(["--wake-stream", &self.wake_stream])
            .args(["--consumer", "e2e-resume"])
            .args(["--sink-timeout-secs", "2", "--ack-wait-secs", "3"])
            .args(["--sweep-secs", "1", "--deadline-grace-secs", "0"])
            .args(["--max-redispatches", "1", "--retry-after-secs", "1"])
            .args(["--reannounce-after-secs", "2"])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn comp-resume");
        Resumer { child }
    }

    fn resume_log(&self) -> String {
        std::fs::read_to_string(self.logs.path().join("resume.log")).unwrap_or_default()
    }

    fn status_of(&self, session: &str, ticket: &str) -> String {
        let log = self.ok("oplog", json!({ "session": session, "after": null, "limit": 100 }));
        log.as_array()
            .unwrap()
            .iter()
            .find(|e| e["ticket"] == ticket)
            .map(|e| e["status"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

/// The whole path, with nothing mocked but the sink: park and wake through the
/// component chain, `comp-park` announcing on `PARK_WAKE`, a real `comp-resume`
/// delivering it, and the same resumer finding a call nobody answered.
#[test]
fn comp_resume_delivers_an_answer_and_gives_up_on_a_call_nobody_answered() {
    let name = "comp_resume_delivers_an_answer_and_gives_up_on_a_call_nobody_answered";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    let _resumer = s.resumer(&sink.url());

    let session = format!("s-{}", nanos());
    let answered = s
        .ok("park", json!({ "session": session, "call": call_of("req-1"), "by": agent("a1") }))
        ["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    let unanswered = s.ok(
        "park",
        json!({ "session": session, "call": call_replaying("pay-1", "unsafe", Some(now_ms() + 500)),
                "by": agent("a1") }),
    )["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    s.ok(
        "wake",
        json!({ "correlation": "req-1", "answer": { "ok": true, "body": [42], "detail": null } }),
    );

    wait_for("both deliveries", 40, || sink.seen().len() >= 2);
    let seen = sink.seen();
    let get = |ticket: &str| seen.iter().find(|(_, b)| b["ticket"] == ticket).cloned().unwrap();

    let (key, body) = get(&answered);
    assert_eq!(key, format!("{answered}:answered"));
    assert_eq!(
        (body["event"].as_str(), body["session"].as_str()),
        (Some("answered"), Some(session.as_str()))
    );
    assert_eq!(body["correlation"], "req-1");
    assert_eq!(body["answer"], json!({ "ok": true, "body": [42], "detail": null }));

    let (key, body) = get(&unanswered);
    assert_eq!(key, format!("{unanswered}:interrupted"));
    assert_eq!(body["event"], "interrupted", "{body}");
    assert!(body["interruption"]["reason"].as_str().unwrap().contains("not replay-safe"), "{body}");
    assert_eq!(body["interruption"]["by"]["id"], "comp-resume");

    // Delivered means closed: both are taken, and a take now says so.
    wait_for("both tickets closed", 20, || {
        s.status_of(&session, &answered) == "resumed"
            && s.status_of(&session, &unanswered) == "resumed"
    });
    assert_eq!(sink.seen().len(), 2, "each told once: {:?}\n{}", sink.seen(), s.resume_log());
}

/// The reason delivery is at least once: `comp-resume` is killed outright while
/// its sink holds the answer, a replacement takes over, and the sink is handed
/// the same delivery again.
#[test]
fn a_comp_resume_killed_mid_delivery_is_replaced_without_losing_the_answer() {
    let name = "a_comp_resume_killed_mid_delivery_is_replaced_without_losing_the_answer";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    sink.set(Mode::Hang);
    let mut first = s.resumer(&sink.url());

    let session = format!("s-{}", nanos());
    let ticket = s
        .ok("park", json!({ "session": session, "call": call_of("req-1"), "by": agent("a1") }))
        ["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    s.ok(
        "wake",
        json!({ "correlation": "req-1", "answer": { "ok": true, "body": [7], "detail": null } }),
    );

    wait_for("the sink to be holding the delivery", 40, || !sink.seen().is_empty());
    first.child.kill().expect("kill -9 the resumer");
    let _ = first.child.wait();
    assert_eq!(s.status_of(&session, &ticket), "ready", "the answer must still be there");

    sink.set(Mode::Accept);
    let _second = s.resumer(&sink.url());
    // The server hands the message to the replacement once `ack-wait` (3s) runs out on the dead one.
    wait_for("the replacement to deliver", 60, || sink.seen().len() >= 2);
    let seen = sink.seen();
    assert_eq!(seen[0].0, seen[1].0, "same delivery, same key: {seen:?}");
    assert_eq!(seen[1].1["answer"]["body"], json!([7]));

    wait_for("the ticket to close", 20, || s.status_of(&session, &ticket) == "resumed");
}

#[test]
fn an_interrupt_made_through_the_component_chain_reaches_the_resumer() {
    let name = "an_interrupt_made_through_the_component_chain_reaches_the_resumer";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    let _resumer = s.resumer(&sink.url());

    // A restarted turn that will not wait for a call it must not repeat says so; `comp-park`
    // announces it, and the resumer tells the turn — the pi-durable "interrupted result".
    let session = format!("s-{}", nanos());
    let ticket = s.ok(
        "park",
        json!({ "session": session, "call": call_replaying("pay-1", "unsafe", None), "by": agent("a1") }),
    )["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    s.ok(
        "interrupt",
        json!({ "ticket": ticket, "reason": "restarted mid-call", "by": agent("turn") }),
    );

    wait_for("the interruption to be delivered", 40, || !sink.seen().is_empty());
    let (_, body) = sink.seen()[0].clone();
    assert_eq!(body["event"], "interrupted");
    assert_eq!(body["interruption"]["reason"], "restarted mid-call");
    assert_eq!(body["interruption"]["by"]["id"], "turn");
}

// ---- ADR-0109: groups ----------------------------------------------------------------------------

fn group_json(correlation: &str, policy: &str, children: u32) -> Value {
    json!({ "correlation": correlation, "description": format!("group {correlation}"),
            "deadline": null, "poll": null, "replay": "unsafe",
            "group": { "policy": policy, "children": children } })
}

fn child_json(correlation: &str, parent: &str) -> Value {
    json!({ "correlation": correlation, "description": format!("child {correlation}"),
            "deadline": null, "poll": null, "replay": "unsafe", "parent": parent })
}

/// A fan-out, end to end: the group and its children parked through the real components, the
/// far side answering each child, and `comp-resume` delivering the group once — as `joined`, with
/// every child — and no child on its own.
#[test]
fn a_fan_out_is_delivered_by_comp_resume_as_one_joined_event() {
    let name = "a_fan_out_is_delivered_by_comp_resume_as_one_joined_event";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    let _resumer = s.resumer(&sink.url());

    let session = format!("s-{}", nanos());
    let park =
        |call: Value| s.ok("park", json!({ "session": session, "call": call, "by": agent("a1") }));
    let g = park(group_json("g", "all-settled", 2))["ticket"].as_str().unwrap().to_string();
    for c in ["c1", "c2"] {
        park(child_json(c, "g"));
    }

    // The entries say who belongs to whom.
    let log = s.ok("oplog", json!({ "session": session, "after": null, "limit": 10 }));
    let by =
        |c: &str| log.as_array().unwrap().iter().find(|e| e["correlation"] == c).unwrap().clone();
    assert_eq!(by("g")["group"], json!({ "policy": "all-settled", "children": 2 }));
    assert_eq!(by("c1")["parent"], json!(g));

    s.ok(
        "wake",
        json!({ "correlation": "c2", "answer": { "ok": true, "body": [2], "detail": null } }),
    );
    assert!(sink.seen().is_empty(), "one answer of two resolves nothing");
    s.ok(
        "wake",
        json!({ "correlation": "c1", "answer": { "ok": true, "body": [1], "detail": null } }),
    );

    wait_for("the group to be delivered", 40, || !sink.seen().is_empty());
    let (key, body) = sink.seen()[0].clone();
    assert_eq!(key, format!("{g}:joined"));
    assert_eq!(body["event"], "joined", "{body}");
    assert_eq!(body["correlation"], "g");
    assert_eq!(body["joined"]["outcome"], "all-answered");
    assert_eq!(body["joined"]["missing"], 0);
    let kids = body["joined"]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 2);
    assert_eq!(
        (kids[0]["correlation"].as_str(), kids[1]["correlation"].as_str()),
        (Some("c1"), Some("c2")),
        "in the order they were parked, not the order they answered"
    );
    assert_eq!(
        kids[0]["ended"],
        json!({ "answered": { "ok": true, "body": [1], "detail": null } })
    );

    // The group is taken by the delivery, and its children were consumed with it.
    wait_for("the group to close", 20, || s.status_of(&session, &g) == "resumed");
    for c in ["c1", "c2"] {
        let t = by(c)["ticket"].as_str().unwrap().to_string();
        assert_eq!(s.status_of(&session, &t), "resumed", "{c}");
    }
    std::thread::sleep(Duration::from_millis(1_500)); // several sweeps: nobody is told twice
    assert_eq!(sink.seen().len(), 1, "one delivery for the whole fan-out: {:?}", sink.seen());
}

/// A failure under fail-fast, through the real chain: the interrupt of one child resolves the
/// group, the siblings are cancelled, and `comp-park` announced the group (not the child) for
/// `comp-resume` to deliver.
#[test]
fn a_fail_fast_group_through_the_chain_cancels_the_siblings_and_is_delivered() {
    let name = "a_fail_fast_group_through_the_chain_cancels_the_siblings_and_is_delivered";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    let _resumer = s.resumer(&sink.url());

    let session = format!("s-{}", nanos());
    let park =
        |call: Value| s.ok("park", json!({ "session": session, "call": call, "by": agent("a1") }));
    let g = park(group_json("g", "fail-fast", 3))["ticket"].as_str().unwrap().to_string();
    let mut kids = Vec::new();
    for c in ["c1", "c2", "c3"] {
        kids.push(park(child_json(c, "g"))["ticket"].as_str().unwrap().to_string());
    }

    s.ok("interrupt", json!({ "ticket": kids[0], "reason": "boom", "by": agent("turn") }));

    wait_for("the group to be delivered", 40, || !sink.seen().is_empty());
    let (_, body) = sink.seen()[0].clone();
    assert_eq!(body["event"], "joined", "{body}");
    assert_eq!(body["joined"]["outcome"], "failed-fast");
    let ended: Vec<Value> =
        body["joined"]["children"].as_array().unwrap().iter().map(|c| c["ended"].clone()).collect();
    assert!(ended[0]["interrupted"]["reason"] == "boom", "{ended:?}");
    assert_eq!((&ended[1], &ended[2]), (&json!("cancelled"), &json!("cancelled")));
    wait_for("the siblings to be cancelled", 20, || {
        s.status_of(&session, &kids[1]) == "cancelled"
            && s.status_of(&session, &kids[2]) == "cancelled"
    });

    // The answer a cancelled sibling sends anyway is accepted and dropped.
    let (status, _) = s.call(
        "wake",
        json!({ "correlation": "c2", "answer": { "ok": true, "body": [], "detail": null } }),
    );
    assert_eq!(status, 200);
    assert_eq!(s.status_of(&session, &kids[1]), "cancelled");
    let _ = g;
}

#[test]
fn what_a_group_and_a_child_refuse_is_refused_through_the_chain() {
    let name = "what_a_group_and_a_child_refuse_is_refused_through_the_chain";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };

    let session = format!("s-{}", nanos());
    let try_park = |call: Value| {
        s.call("park", json!({ "session": session, "call": call, "by": agent("a1") }))
    };
    let invalid = |(status, v): (u16, Value), what: &str| {
        assert_eq!((status, v["error"].as_str()), (400, Some("invalid")), "{what}: {v}");
    };

    let g = try_park(group_json("g", "all-settled", 1)).1["ticket"].as_str().unwrap().to_string();
    let c = try_park(child_json("c1", "g")).1["ticket"].as_str().unwrap().to_string();
    invalid(try_park(child_json("c2", "g")), "a child past the declared number");
    invalid(try_park(child_json("c3", "nope")), "a child of nothing");
    invalid(
        try_park(json!({ "correlation": "g2", "description": "d", "deadline": null, "poll": null,
                         "replay": "unsafe", "parent": "g", "group": { "policy": "fail-fast", "children": 1 } })),
        "a group inside a group",
    );
    invalid(try_park(group_json("g0", "fail-fast", 0)), "a group of none");

    invalid(
        s.call(
            "wake",
            json!({ "correlation": "g", "answer": { "ok": true, "body": [], "detail": null } }),
        ),
        "waking a group",
    );
    invalid(
        s.call("interrupt", json!({ "ticket": g, "reason": "x", "by": agent("a1") })),
        "interrupting a group",
    );
    s.ok(
        "wake",
        json!({ "correlation": "c1", "answer": { "ok": true, "body": [], "detail": null } }),
    );
    invalid(s.call("take-ready", json!({ "ticket": c })), "taking a child");

    // The group is what is taken, and it is a `joined`.
    let taken = s.ok("take-ready", json!({ "ticket": g }));
    assert_eq!(taken["joined"]["outcome"], "all-answered", "{taken}");
}

// ---- ADR-0110: commits ----------------------------------------------------------------------------

fn wake_effect(correlation: &str) -> Value {
    json!({ "wake": { "correlation": correlation,
                      "answer": { "ok": true, "body": [1], "detail": null } } })
}

/// A commit through the real chain: a fan-out registered as one unit, the identical commit again
/// found done, a list with one bad effect registering nothing — and a second commit whose `wake`
/// effects resolve the group, so `comp-park` announces it and `comp-resume` delivers it.
#[test]
fn a_commit_registers_a_fan_out_whole_and_its_effects_reach_the_resumer() {
    let name = "a_commit_registers_a_fan_out_whole_and_its_effects_reach_the_resumer";
    let Some(nats) = nats_url(name) else { return };
    let Some(s) = Stack::up(name, &nats) else { return };
    let sink = StubSink::start();
    let _resumer = s.resumer(&sink.url());

    let session = format!("s-{}", nanos());
    let commit = |effects: Value| {
        s.call("commit", json!({ "session": session, "effects": effects, "by": agent("a1") }))
    };
    let fan_out = || {
        json!([
            { "park": group_json("g", "all-settled", 2) },
            { "park": child_json("c1", "g") },
            { "park": child_json("c2", "g") },
        ])
    };

    let (status, done) = commit(fan_out());
    assert_eq!(status, 200, "{done}");
    let parked = done["parked"].as_array().unwrap();
    assert_eq!(parked.len(), 3);
    assert!(parked.iter().all(|p| p["outcome"] == "parked"), "{done}");
    let batch = done["batch"].as_str().unwrap().to_string();
    let count = || {
        s.ok("oplog", json!({ "session": session, "after": null, "limit": 50 }))
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(), 3);

    // The identical list again is the same commit, and says what it found.
    let (status, again) = commit(fan_out());
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["batch"], batch);
    assert!(again["parked"].as_array().unwrap().iter().all(|p| p["outcome"] == "already-parked"));
    assert_eq!(count(), 3);

    // One bad effect, and none of the good ones around it is registered.
    let (status, refused) = commit(json!([
        { "park": call_replaying("fine", "unsafe", None) },
        { "park": child_json("orphan", "no-such-group") },
    ]));
    assert_eq!((status, refused["error"].as_str()), (400, Some("invalid")), "{refused}");
    let (status, refused) = commit(json!([]));
    assert_eq!((status, refused["error"].as_str()), (400, Some("invalid")), "{refused}");
    assert_eq!(count(), 3, "the good effect in front of the bad one was not registered");

    // A commit that answers both children resolves the group; the daemon announces the group,
    // never a child, and the resumer delivers it once.
    assert!(sink.seen().is_empty());
    let (status, answered) = commit(json!([wake_effect("c1"), wake_effect("c2")]));
    assert_eq!(status, 200, "{answered}");
    wait_for("the group to be delivered", 40, || !sink.seen().is_empty());
    let (key, body) = sink.seen()[0].clone();
    assert!(key.ends_with(":joined"), "{key}");
    assert_eq!(body["event"], "joined");
    assert_eq!(body["joined"]["outcome"], "all-answered");
    assert_eq!(body["joined"]["children"].as_array().unwrap().len(), 2);
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(sink.seen().len(), 1, "once: {:?}", sink.seen());
}
