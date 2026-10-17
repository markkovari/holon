//! What survives `kill -9` of NATS itself (ADR-0106, *What survives what*).
//!
//! Everything else in this crate's tests kills a *client* of JetStream. This one kills the
//! server: parks, wakes, interrupts and cancels against a `nats-server -js` this test starts,
//! SIGKILLs it the moment the last write was acknowledged, starts another on the same store
//! directory, and checks that every acknowledged write is there — the tickets, their states,
//! the indexes a webhook and `pending` use, and the `PARK_WAKE` messages nobody had taken.
//!
//! What it proves is exactly this: an acknowledged write survives the death of the server
//! *process*. It does not and cannot prove power loss or an OS crash, which lose what the
//! kernel had not yet flushed; see the ADR for what that costs and what to set.
//!
//! Needs `nats-server` on `PATH`; without it every test prints `SKIPPED` and returns.
#![cfg(feature = "native")]

use std::collections::BTreeSet;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use holon_park::engine::{now_ms, Engine};
use holon_park::model::{
    Agent, CallResult, OutboundCall, ReplayPolicy, Resolution, TurnStatus, WakeMessage,
};
use holon_park::nats;

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(dir: &std::path::Path, port: Option<u16>) -> Option<Server> {
        let port = port.unwrap_or_else(|| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        });
        let child = Command::new("nats-server")
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let t0 = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(t0.elapsed() < Duration::from_secs(15), "nats-server never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(Server { child, port })
    }

    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }

    /// SIGKILL: no flush, no goodbye.
    fn kill(mut self) -> u16 {
        self.child.kill().expect("kill -9 nats-server");
        let _ = self.child.wait();
        self.port
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn call(correlation: &str, replay: ReplayPolicy) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: "restart test".into(),
        deadline: None,
        poll: None,
        replay,
    }
}

fn answer(body: &str) -> CallResult {
    CallResult { ok: true, body: body.as_bytes().to_vec(), detail: None }
}

#[tokio::test]
async fn everything_acknowledged_survives_kill_dash_9_of_nats() {
    // Under the target directory, which Cargo hands every integration test for exactly this: a
    // fresh, private path rather than a guessable one in the system temp dir.
    let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "holon-park-restart-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let Some(server) = Server::start(&tmp, None) else {
        eprintln!(
            "SKIPPED everything_acknowledged_survives_kill_dash_9_of_nats: no `nats-server` on PATH"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let (bucket, stream) = ("restart-test".to_string(), "RESTART_WAKE".to_string());
    let sessions = ["s1", "s2", "s3"];
    let by = Agent::named("turn");

    // ---- before: write, acknowledge, and die ---------------------------------------------
    let (store, wake) = nats::connect(&server.url(), &bucket, &stream).await.unwrap();
    let engine = Engine::new(store);
    let mut expected: Vec<(String, String, TurnStatus)> = Vec::new(); // (session, correlation, status)
    let mut announced = BTreeSet::new();
    for i in 0..30 {
        let session = sessions[i % 3];
        let correlation = format!("c{i}");
        let replay = [ReplayPolicy::Safe, ReplayPolicy::Unsafe, ReplayPolicy::Abandon][i % 3];
        let p = engine
            .park(&session.to_string(), call(&correlation, replay), by.clone(), now_ms())
            .await
            .unwrap();
        let status = match i % 6 {
            // answered, and announced on PARK_WAKE
            0 | 1 => {
                let w = engine.wake(&correlation, answer("42"), now_ms()).await.unwrap();
                wake.publish(&WakeMessage::resolved(&w.session, &w.ticket)).await.unwrap();
                announced.insert(p.ticket.clone());
                TurnStatus::Ready
            }
            // interrupted, and announced
            2 => {
                let o = engine.interrupt(&p.ticket, "gave up", by.clone(), now_ms()).await.unwrap();
                wake.publish(&WakeMessage::resolved(&o.session, &o.ticket)).await.unwrap();
                announced.insert(p.ticket.clone());
                TurnStatus::Interrupted
            }
            3 => {
                engine.cancel(&p.ticket, by.clone(), now_ms()).await.unwrap();
                TurnStatus::Cancelled
            }
            // answered and taken
            4 => {
                engine.wake(&correlation, answer("x"), now_ms()).await.unwrap();
                engine.take_ready(&p.ticket, now_ms()).await.unwrap();
                TurnStatus::Resumed
            }
            _ => TurnStatus::Parked,
        };
        expected.push((session.to_string(), correlation, status));
    }
    // The last acknowledgement is in. Now the server dies, with no warning and no flush.
    drop(engine);
    drop(wake);
    let port = server.kill();

    // ---- after: the same directory, a new process ----------------------------------------
    let server = Server::start(&tmp, Some(port)).expect("restart nats-server");
    let (store, wake) = nats::connect(&server.url(), &bucket, &stream).await.unwrap();
    let engine = Engine::new(store);

    // Every ticket, in the state it was acknowledged in, found by session…
    for session in sessions {
        let log = engine.oplog(&session.to_string(), None, 1000, now_ms()).await.unwrap();
        let mut got: Vec<(String, TurnStatus)> =
            log.iter().map(|e| (e.correlation.clone(), e.status)).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want: Vec<(String, TurnStatus)> = expected
            .iter()
            .filter(|(s, ..)| s == session)
            .map(|(_, c, st)| (c.clone(), *st))
            .collect();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, want, "session {session} after the restart");
    }
    // …and by the correlation a webhook presents: a parked call can still be answered.
    let (session, correlation, _) =
        expected.iter().find(|(.., st)| *st == TurnStatus::Parked).unwrap().clone();
    let w = engine.wake(&correlation, answer("late"), now_ms()).await.unwrap();
    assert!(w.freshly_woken);
    assert_eq!(w.session, session);
    assert!(matches!(
        engine.take_ready(&w.ticket, now_ms()).await.unwrap(),
        Resolution::Answered(a) if a.body == b"late"
    ));

    // The announcements nobody had taken are still queued, each exactly once.
    let mut consumer = wake.consumer("verify", Duration::from_secs(30)).await.unwrap();
    let mut queued = BTreeSet::new();
    let t0 = Instant::now();
    while queued.len() < announced.len() && t0.elapsed() < Duration::from_secs(15) {
        if let Ok(Ok(Some(d))) = tokio::time::timeout(Duration::from_secs(2), consumer.next()).await
        {
            assert!(queued.insert(d.wake.ticket.clone()), "{} queued twice", d.wake.ticket);
            d.ack().await.unwrap();
        }
    }
    assert_eq!(queued, announced, "PARK_WAKE lost or invented a message across the kill");

    // What the ADR says about where the data lives, read off the server rather than the source:
    // both the ticket bucket and `PARK_WAKE` are on disk, and neither is replicated (one node).
    let js = async_nats::jetstream::new(async_nats::connect(&server.url()).await.unwrap());
    for name in [format!("KV_{bucket}"), stream.clone()] {
        let cfg = js.get_stream(&name).await.unwrap().get_info().await.unwrap().config;
        assert_eq!(cfg.storage, async_nats::jetstream::stream::StorageType::File, "{name}");
        assert_eq!(cfg.num_replicas, 1, "{name}: one copy, on this server's disk");
    }

    drop(engine);
    drop(wake);
    drop(server);
    let _ = std::fs::remove_dir_all(&tmp);
}
