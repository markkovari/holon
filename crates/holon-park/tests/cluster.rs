//! What `replicas` buys, and what it refuses (ADR-0111).
//!
//! `tests/restart.rs` kills the one server there is and brings it back. That says nothing about
//! a server that stays dead, and a ticket on a dead node is out of reach until it returns. These
//! tests start a real three-node JetStream cluster, ask for three copies, and SIGKILL the node
//! that leads the bucket — then the one that leads `PARK_WAKE` — while tickets are being written,
//! answered, announced and delivered.
//!
//! What they prove is exactly this: with three copies, every acknowledged write survives the
//! loss of the leader of either, writes and delivery carry on from the two that are left, and the
//! node that was killed catches up when it returns. With one copy, the same kill takes the bucket
//! out of reach. They do not prove a majority lost (two of three), a partition, or a disk lost
//! with its node and never replaced.
//!
//! Needs `nats-server` on `PATH`; without it every test prints `SKIPPED` and returns.
#![cfg(feature = "native")]

use std::collections::BTreeSet;
use std::future::Future;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::{self, stream};
use holon_park::engine::{now_ms, Engine};
use holon_park::error::ParkError;
use holon_park::model::{
    Agent, CallResult, OutboundCall, ReplayPolicy, Resolution, TurnStatus, WakeMessage,
};
use holon_park::nats::{self, NatsParkStore, WakeStream};
use holon_park::resume::{run, Delivery, RunConfig, Sink, SinkReply};

/// How long a write is retried while JetStream elects a new leader. Generous: the point is what
/// is lost, not how fast it comes back, and a slow CI box should not turn a pass into a flake.
const PATIENCE: Duration = Duration::from_secs(60);

// ---- a cluster of real servers ---------------------------------------------------------------

struct Node {
    /// The server name, which is what a stream's cluster info calls a leader.
    name: String,
    port: u16,
    route: u16,
    dir: PathBuf,
    child: Option<Child>,
}

struct Cluster {
    root: PathBuf,
    nodes: Vec<Node>,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Cluster {
    /// `n` servers: one on its own, or `n` that route to each other. `None` without `nats-server`.
    fn start(what: &str, n: usize) -> Option<Cluster> {
        // Under the target directory, which Cargo hands every integration test for exactly this:
        // a fresh, private path rather than a guessable one in the system temp dir.
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "holon-park-cluster-{what}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let nodes = (0..n)
            .map(|i| {
                let name = format!("node-{}", (b'a' + i as u8) as char);
                let dir = root.join(&name);
                std::fs::create_dir_all(&dir).unwrap();
                Node { name, port: free_port(), route: free_port(), dir, child: None }
            })
            .collect();
        let mut cluster = Cluster { root, nodes };
        for i in 0..n {
            if !cluster.spawn(i) {
                return None; // `Drop` stops the ones already started and removes the directory
            }
        }
        for node in &cluster.nodes {
            let t0 = Instant::now();
            while TcpStream::connect(("127.0.0.1", node.port)).is_err() {
                assert!(t0.elapsed() < Duration::from_secs(15), "{} never listened", node.name);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Some(cluster)
    }

    /// Start (or restart, on the same directory and the same ports) the `i`th server.
    fn spawn(&mut self, i: usize) -> bool {
        let clustered = self.nodes.len() > 1;
        let routes: Vec<String> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, o)| format!("nats://127.0.0.1:{}", o.route))
            .collect();
        let node = &mut self.nodes[i];
        let log = std::fs::File::create(node.dir.join("server.log")).unwrap();
        let mut cmd = Command::new("nats-server");
        cmd.args(["-js", "-n", &node.name, "-a", "127.0.0.1", "-p", &node.port.to_string(), "-sd"])
            .arg(node.dir.join("store"));
        if clustered {
            cmd.args(["--cluster_name", "holon-park-test"])
                .args(["--cluster", &format!("nats://127.0.0.1:{}", node.route)])
                .args(["--routes", &routes.join(",")]);
        }
        match cmd.stdout(Stdio::null()).stderr(log).spawn() {
            Ok(child) => {
                node.child = Some(child);
                true
            }
            Err(_) => false,
        }
    }

    fn index(&self, name: &str) -> usize {
        self.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"))
    }

    /// SIGKILL: no flush, no goodbye, and no coming back until `restart`.
    fn kill(&mut self, name: &str) {
        let i = self.index(name);
        let mut child = self.nodes[i].child.take().unwrap_or_else(|| panic!("{name} is not up"));
        child.kill().expect("kill -9 nats-server");
        let _ = child.wait();
    }

    fn restart(&mut self, name: &str) {
        let i = self.index(name);
        assert!(self.nodes[i].child.is_none(), "{name} is still up");
        assert!(self.spawn(i), "restart {name}");
    }

    /// Every server, comma-separated, the way `--nats-url` takes them.
    fn urls(&self) -> String {
        self.nodes
            .iter()
            .map(|n| format!("nats://127.0.0.1:{}", n.port))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// A JetStream client to look at the cluster with, which follows it when a node dies.
    async fn inspector(&self) -> jetstream::Context {
        let servers: Vec<async_nats::ServerAddr> =
            self.urls().split(',').map(|u| u.parse().unwrap()).collect();
        jetstream::new(async_nats::connect(servers).await.expect("inspector connect"))
    }

    /// Wait until `copies` servers can hold a stream between them: a cluster whose ports are
    /// open has not necessarily elected anything yet.
    async fn ready(&self, copies: usize) {
        let js = self.inspector().await;
        let t0 = Instant::now();
        loop {
            let probe = stream::Config {
                name: "READY".into(),
                subjects: vec!["ready.probe".into()],
                storage: stream::StorageType::Memory,
                num_replicas: copies,
                ..Default::default()
            };
            match js.create_stream(probe).await {
                Ok(_) => {
                    js.delete_stream("READY").await.expect("delete the probe stream");
                    return;
                }
                Err(e) => {
                    assert!(t0.elapsed() < Duration::from_secs(30), "cluster never ready: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for node in &mut self.nodes {
            if let Some(child) = node.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        // A failing test keeps what the servers said, which is the only account of a flake.
        if std::thread::panicking() {
            eprintln!("server logs kept in {}", self.root.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

macro_rules! cluster_or_skip {
    ($what:expr, $n:expr) => {
        match Cluster::start($what, $n) {
            Some(c) => c,
            None => {
                eprintln!("SKIPPED {}: no `nats-server` on PATH", $what);
                return;
            }
        }
    };
}

// ---- looking at where a stream lives ---------------------------------------------------------

struct Placement {
    copies: usize,
    leader: Option<String>,
    /// (server, caught up)
    followers: Vec<(String, bool)>,
}

async fn placement(js: &jetstream::Context, name: &str) -> Option<Placement> {
    let info = js.get_stream(name).await.ok()?.get_info().await.ok()?;
    let cluster = info.cluster?;
    Some(Placement {
        copies: info.config.num_replicas,
        leader: cluster.leader,
        followers: cluster
            .replicas
            .into_iter()
            .map(|p| (p.name, p.current && !p.offline))
            .collect(),
    })
}

/// Wait until `name` has a leader and `copies - 1` followers, all caught up.
async fn settled(js: &jetstream::Context, name: &str, copies: usize) -> Placement {
    let t0 = Instant::now();
    loop {
        if let Some(p) = placement(js, name).await {
            if p.leader.is_some()
                && p.followers.len() == copies - 1
                && p.followers.iter().all(|(_, current)| *current)
            {
                return p;
            }
        }
        assert!(t0.elapsed() < PATIENCE, "{name} never settled on {copies} copies");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Retry `f` while the store says `Storage`, which is what a request that lands in the middle of
/// an election looks like; anything else is the answer. The same call is repeated, which is only
/// safe because the engine's calls are idempotent — the property the crash matrix is about.
async fn patient<T, Fut>(mut f: impl FnMut() -> Fut) -> Result<T, ParkError>
where
    Fut: Future<Output = Result<T, ParkError>>,
{
    let t0 = Instant::now();
    loop {
        match f().await {
            Err(ParkError::Storage(_)) if t0.elapsed() < PATIENCE => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            other => return other,
        }
    }
}

// ---- tickets ---------------------------------------------------------------------------------

const SESSIONS: [&str; 3] = ["s1", "s2", "s3"];

fn call(correlation: &str, replay: ReplayPolicy) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: "cluster test".into(),
        deadline: None,
        poll: None,
        replay,
        parent: None,
        group: None,
    }
}

fn answer(body: &str) -> CallResult {
    CallResult { ok: true, body: body.as_bytes().to_vec(), detail: None }
}

/// What the store acknowledged, to be asked for again after a node is lost.
#[derive(Default)]
struct Ledger {
    /// (session, correlation, status)
    expected: Vec<(String, String, TurnStatus)>,
    /// Tickets announced on `PARK_WAKE`, which nobody has taken.
    announced: BTreeSet<String>,
}

/// Park tickets `range` and leave them in a mix of states, as `tests/restart.rs` does — each
/// write retried through an election, and recorded in the ledger only once it was acknowledged.
async fn write_mix(
    engine: &Engine<NatsParkStore>,
    wake: &WakeStream,
    ledger: &mut Ledger,
    range: std::ops::Range<usize>,
) {
    let by = Agent::named("turn");
    for i in range {
        let session = SESSIONS[i % 3].to_string();
        let correlation = format!("c{i}");
        let replay = [ReplayPolicy::Safe, ReplayPolicy::Unsafe, ReplayPolicy::Abandon][i % 3];
        let p = patient(|| engine.park(&session, call(&correlation, replay), by.clone(), now_ms()))
            .await
            .unwrap();
        let status = match i % 6 {
            0 | 1 => {
                let w =
                    patient(|| engine.wake(&correlation, answer("42"), now_ms())).await.unwrap();
                let msg = WakeMessage::resolved(&w.session, &w.ticket);
                patient(|| wake.publish(&msg)).await.unwrap();
                ledger.announced.insert(p.ticket.clone());
                TurnStatus::Ready
            }
            2 => {
                let o = patient(|| engine.interrupt(&p.ticket, "gave up", by.clone(), now_ms()))
                    .await
                    .unwrap();
                let msg = WakeMessage::resolved(&o.session, &o.ticket);
                patient(|| wake.publish(&msg)).await.unwrap();
                ledger.announced.insert(p.ticket.clone());
                TurnStatus::Interrupted
            }
            3 => {
                // A write acknowledged and then repeated is `already closed`: that is the first
                // attempt having landed after all.
                match patient(|| engine.cancel(&p.ticket, by.clone(), now_ms())).await {
                    Ok(_) | Err(ParkError::AlreadyClosed(_)) => {}
                    Err(e) => panic!("cancel c{i}: {e}"),
                }
                TurnStatus::Cancelled
            }
            4 => {
                patient(|| engine.wake(&correlation, answer("x"), now_ms())).await.unwrap();
                match patient(|| engine.take_ready(&p.ticket, now_ms())).await {
                    Ok(_) | Err(ParkError::AlreadyClosed(_)) => {}
                    Err(e) => panic!("take c{i}: {e}"),
                }
                TurnStatus::Resumed
            }
            _ => TurnStatus::Parked,
        };
        ledger.expected.push((session, correlation, status));
    }
}

/// Every ticket in the ledger, in the state it was acknowledged in, found by its session.
async fn verify(engine: &Engine<NatsParkStore>, ledger: &Ledger, when: &str) {
    for session in SESSIONS {
        let owned = session.to_string();
        let log = patient(|| engine.oplog(&owned, None, 1000, now_ms())).await.unwrap();
        let mut got: Vec<(String, TurnStatus)> =
            log.iter().map(|e| (e.correlation.clone(), e.status)).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want: Vec<(String, TurnStatus)> = ledger
            .expected
            .iter()
            .filter(|(s, ..)| s == session)
            .map(|(_, c, st)| (c.clone(), *st))
            .collect();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, want, "session {session} {when}");
    }
}

/// One more write, and how long it took to be acknowledged: the time a caller that retries sees
/// the bucket gone for. An upper bound on the leaderless period and not a measure of it: a
/// request to a stream with no leader is not answered, so each refused attempt waits out the
/// client's own five-second JetStream timeout, and this is about five seconds, about ten, or
/// nothing at all if the node that died led something else.
async fn first_write_after(
    engine: &Engine<NatsParkStore>,
    ledger: &mut Ledger,
    correlation: &str,
) -> Duration {
    let session = SESSIONS[0].to_string();
    let t0 = Instant::now();
    patient(|| {
        engine.park(
            &session,
            call(correlation, ReplayPolicy::Unsafe),
            Agent::named("turn"),
            now_ms(),
        )
    })
    .await
    .unwrap();
    let took = t0.elapsed();
    ledger.expected.push((session, correlation.into(), TurnStatus::Parked));
    took
}

/// One more ticket, parked and answered, and how long until its announcement was acknowledged by
/// `PARK_WAKE`: what a caller that answers a call sees of that stream being gone.
async fn first_announcement_after(
    engine: &Engine<NatsParkStore>,
    wake: &WakeStream,
    ledger: &mut Ledger,
    correlation: &str,
) -> Duration {
    let (session, by) = (SESSIONS[1].to_string(), Agent::named("turn"));
    let t0 = Instant::now();
    let p = patient(|| {
        engine.park(&session, call(correlation, ReplayPolicy::Unsafe), by.clone(), now_ms())
    })
    .await
    .unwrap();
    let w = patient(|| engine.wake(correlation, answer("42"), now_ms())).await.unwrap();
    let msg = WakeMessage::resolved(&w.session, &w.ticket);
    patient(|| wake.publish(&msg)).await.unwrap();
    let took = t0.elapsed();
    ledger.announced.insert(p.ticket);
    ledger.expected.push((session, correlation.into(), TurnStatus::Ready));
    took
}

fn names() -> (String, String) {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    (format!("cluster-test-{n}"), format!("CLUSTER_WAKE_{n}"))
}

// ---- the tests -------------------------------------------------------------------------------

#[tokio::test]
async fn three_copies_survive_losing_the_leader_of_either_stream() {
    let mut cluster = cluster_or_skip!("three_copies", 3);
    cluster.ready(3).await;
    let (bucket, wake_stream) = names();
    let kv_stream = format!("KV_{bucket}");

    let (store, wake) =
        nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 3).await.unwrap();
    let engine = Engine::new(store);
    let js = cluster.inspector().await;

    // What was asked for is what the server made, read off it rather than off the source.
    for name in [&kv_stream, &wake_stream] {
        let p = settled(&js, name, 3).await;
        let info = js.get_stream(name).await.unwrap().get_info().await.unwrap();
        assert_eq!(p.copies, 3, "{name}");
        assert_eq!(info.config.storage, stream::StorageType::File, "{name}");
    }

    let mut ledger = Ledger::default();
    write_mix(&engine, &wake, &mut ledger, 0..30).await;
    verify(&engine, &ledger, "before anything was lost").await;

    // ---- the node that leads the bucket dies ---------------------------------------------
    let victim = placement(&js, &kv_stream).await.unwrap().leader.expect("a leader");
    cluster.kill(&victim);
    let back = first_write_after(&engine, &mut ledger, "after-kill-1").await;
    eprintln!("bucket leader {victim} killed: a write was acknowledged after {back:?}");
    verify(&engine, &ledger, &format!("after the bucket's leader {victim} was killed")).await;
    write_mix(&engine, &wake, &mut ledger, 30..42).await; // answered, announced, taken: all with two copies
    verify(&engine, &ledger, "after writing through the loss").await;

    // The node comes back, on the same directory, and catches up rather than being rebuilt.
    cluster.restart(&victim);
    for name in [&kv_stream, &wake_stream] {
        settled(&js, name, 3).await;
    }
    verify(&engine, &ledger, &format!("after {victim} returned")).await;

    // ---- then the node that leads PARK_WAKE dies -----------------------------------------
    let victim = placement(&js, &wake_stream).await.unwrap().leader.expect("a leader");
    cluster.kill(&victim);
    let back = first_announcement_after(&engine, &wake, &mut ledger, "after-kill-2").await;
    eprintln!("PARK_WAKE leader {victim} killed: an announcement was acknowledged after {back:?}");
    write_mix(&engine, &wake, &mut ledger, 42..54).await;
    verify(&engine, &ledger, &format!("after PARK_WAKE's leader {victim} was killed")).await;

    // A parked call can still be answered by the correlation a webhook presents.
    let (session, correlation, _) =
        ledger.expected.iter().find(|(.., st)| *st == TurnStatus::Parked).unwrap().clone();
    let w = patient(|| engine.wake(&correlation, answer("late"), now_ms())).await.unwrap();
    assert_eq!(w.session, session);
    assert!(matches!(
        patient(|| engine.take_ready(&w.ticket, now_ms())).await.unwrap(),
        Resolution::Answered(a) if a.body == b"late"
    ));

    // Every announcement nobody had taken is still queued, once each, across both losses.
    //
    // Creating a consumer goes through JetStream's meta leader, which may be the node just killed:
    // it is refused for five seconds or so until another is elected (measured: ADR-0111), so it is
    // retried like any other call. Without the retry this test panicked here about once in thirty
    // runs.
    let mut consumer = patient(|| wake.consumer("verify", Duration::from_secs(30))).await.unwrap();
    let mut queued = BTreeSet::new();
    let t0 = Instant::now();
    while queued.len() < ledger.announced.len() && t0.elapsed() < PATIENCE {
        if let Ok(Ok(Some(d))) = tokio::time::timeout(Duration::from_secs(3), consumer.next()).await
        {
            // A message sent back and delivered again is the same message; two messages for one
            // ticket are not, and are the thing this is looking for.
            let first = d.delivered == 1;
            let ticket = d.wake.ticket.clone();
            assert!(queued.insert(ticket.clone()) || !first, "{ticket} queued twice");
            let _ = d.ack().await;
        }
    }
    assert_eq!(queued, ledger.announced, "PARK_WAKE lost or invented a message across the kills");
}

/// The engine reads a ticket and its indexes straight after writing them, and often through a
/// different connection: `wake` finds the ticket by its correlation the moment a webhook arrives.
/// A bucket's reads are direct gets, which any current copy may answer, so on three copies a read
/// could in principle be older than the write before it. Here every call is parked through one
/// server, answered through the next and listed and taken through the third, each told about that
/// server alone, straight away, three hundred times over.
///
/// This is the steady state, where it was also measured with a throwaway probe: twelve thousand
/// reads, each right after a write and through each node in turn, none older. A failover window is
/// not probed for staleness.
#[tokio::test]
async fn a_call_parked_through_one_node_is_answered_through_another_at_once() {
    let cluster = cluster_or_skip!("cross_node", 3);
    cluster.ready(3).await;
    let (bucket, wake_stream) = names();

    let mut engines = Vec::new();
    for node in &cluster.nodes {
        let url = format!("nats://127.0.0.1:{}", node.port);
        let (store, _) = nats::connect_with_replicas(&url, &bucket, &wake_stream, 3).await.unwrap();
        engines.push(Engine::new(store));
    }

    let by = Agent::named("turn");
    for i in 0..300usize {
        let (parks, answers, lists) =
            (&engines[i % 3], &engines[(i + 1) % 3], &engines[(i + 2) % 3]);
        let session = format!("s{}", i % 10);
        let correlation = format!("x{i}");
        let p = parks
            .park(&session, call(&correlation, ReplayPolicy::Unsafe), by.clone(), now_ms())
            .await
            .unwrap();
        answers
            .wake(&correlation, answer("42"), now_ms())
            .await
            .unwrap_or_else(|e| panic!("{correlation}, answered through another node: {e}"));
        let log = lists.oplog(&session, None, 1000, now_ms()).await.unwrap();
        let entry = log.iter().find(|e| e.ticket == p.ticket);
        assert_eq!(
            entry.map(|e| e.status),
            Some(TurnStatus::Ready),
            "{correlation}, listed through a third node"
        );
        lists.take_ready(&p.ticket, now_ms()).await.unwrap();
    }
}

/// What a sink saw.
#[derive(Default)]
struct Rec(Mutex<Vec<Delivery>>);

impl Rec {
    fn tickets(&self) -> BTreeSet<String> {
        self.0.lock().unwrap().iter().map(|d| d.ticket.clone()).collect()
    }
}

impl Sink for Rec {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        self.0.lock().unwrap().push(d.clone());
        SinkReply::Delivered
    }
}

#[tokio::test]
async fn delivery_goes_on_through_losing_the_node_that_leads_the_consumer() {
    let mut cluster = cluster_or_skip!("delivery", 3);
    cluster.ready(3).await;
    let (bucket, wake_stream) = names();

    let (store, wake) =
        nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 3).await.unwrap();
    let (engine, wake) = (Arc::new(Engine::new(store)), Arc::new(wake));
    let js = cluster.inspector().await;
    let sink = Arc::new(Rec::default());
    let cfg = RunConfig {
        consumer: "resumer".into(),
        ack_wait: Duration::from_secs(2),
        concurrency: 2,
        sweep_every: Duration::from_millis(300),
        ..Default::default()
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let resumer = tokio::spawn(run(engine.clone(), wake.clone(), sink.clone(), cfg, rx));

    let by = Agent::named("turn");
    let session = "s-delivery".to_string();
    let mut tickets = BTreeSet::new();
    let answered = |range: std::ops::Range<usize>| {
        let (engine, wake, by, session) =
            (engine.clone(), wake.clone(), by.clone(), session.clone());
        async move {
            let mut out = Vec::new();
            for i in range {
                let c = format!("d{i}");
                let p = patient(|| {
                    engine.park(&session, call(&c, ReplayPolicy::Unsafe), by.clone(), now_ms())
                })
                .await
                .unwrap();
                let w = patient(|| engine.wake(&c, answer("42"), now_ms())).await.unwrap();
                let msg = WakeMessage::resolved(&w.session, &w.ticket);
                patient(|| wake.publish(&msg)).await.unwrap();
                out.push(p.ticket);
            }
            out
        }
    };

    tickets.extend(answered(0..6).await);
    wait_for("the first six to be delivered", || sink.tickets().len() == 6).await;

    // The consumer is a raft group of its own, and may be led from a node that leads neither
    // stream: kill the one that leads *it*, which is the node a delivery depends on.
    let leader = {
        let stream = js.get_stream(&wake_stream).await.unwrap();
        let consumer: async_nats::jetstream::consumer::PullConsumer =
            stream.get_consumer("resumer").await.unwrap();
        let info = consumer.get_info().await.unwrap();
        info.cluster.as_ref().and_then(|c| c.leader.clone()).expect("a leader")
    };
    cluster.kill(&leader);
    eprintln!("consumer leader {leader} killed mid-run");

    tickets.extend(answered(6..12).await);
    wait_for("all twelve to be delivered", || sink.tickets() == tickets).await;
    assert_eq!(sink.tickets(), tickets, "every answered call reached the sink");

    // Delivered means closed: the sink said yes, so each ticket was taken.
    let log = patient(|| engine.oplog(&session, None, 100, now_ms())).await.unwrap();
    let t0 = Instant::now();
    let mut open = log.iter().filter(|e| e.status != TurnStatus::Resumed).count();
    while open > 0 && t0.elapsed() < PATIENCE {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let log = patient(|| engine.oplog(&session, None, 100, now_ms())).await.unwrap();
        open = log.iter().filter(|e| e.status != TurnStatus::Resumed).count();
    }
    assert_eq!(open, 0, "a delivered ticket was left open");

    let _ = stop.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), resumer).await;
}

async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < PATIENCE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn one_copy_is_out_of_reach_while_its_node_is_down() {
    let mut cluster = cluster_or_skip!("one_copy", 3);
    cluster.ready(3).await;
    let (bucket, wake_stream) = names();

    // Asked for nothing, which is what every deployment before the flag had.
    let (store, _wake) = nats::connect(&cluster.urls(), &bucket, &wake_stream).await.unwrap();
    let engine = Engine::new(store);
    let js = cluster.inspector().await;
    let session = "s-one".to_string();
    patient(|| {
        engine.park(&session, call("only", ReplayPolicy::Unsafe), Agent::named("turn"), now_ms())
    })
    .await
    .unwrap();

    let kv_stream = format!("KV_{bucket}");
    let p = placement(&js, &kv_stream).await.unwrap();
    assert_eq!((p.copies, p.followers.len()), (1, 0), "one copy, on one server");
    let holder = p.leader.expect("a holder");
    cluster.kill(&holder);

    // The two servers that are left are fine and have nothing to say about it.
    let t0 = Instant::now();
    let err = engine.oplog(&session, None, 10, now_ms()).await.expect_err("the bucket is gone");
    eprintln!("{holder} killed: reading the bucket fails after {:?}: {err}", t0.elapsed());
    assert!(matches!(err, ParkError::Storage(_)), "{err}");

    // It was only out of reach: the node returns, and so does the ticket.
    cluster.restart(&holder);
    let log = patient(|| engine.oplog(&session, None, 10, now_ms())).await.unwrap();
    assert_eq!(log.len(), 1);
}

#[tokio::test]
async fn an_existing_bucket_with_fewer_copies_is_refused_and_one_with_more_is_not() {
    let cluster = cluster_or_skip!("refused", 3);
    cluster.ready(3).await;
    let (bucket, wake_stream) = names();
    let js = cluster.inspector().await;

    // Made the way every deployment before the flag made it: one copy of each.
    nats::connect(&cluster.urls(), &bucket, &wake_stream).await.unwrap();

    // Three were asked for and the bucket has one. Not called three, and not changed.
    let err = match nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 3).await {
        Ok(_) => panic!("a bucket with one copy was accepted as three"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains(&format!("bucket `{bucket}`")), "names the bucket: {err}");
    assert!(err.contains(&format!("nats kv edit {bucket} --replicas 3")), "and the remedy: {err}");
    assert_eq!(placement(&js, &format!("KV_{bucket}")).await.unwrap().copies, 1, "left alone");

    // Raised by the operator, the way the message says. The bucket is enough for the bucket's
    // check and the stream is not enough for the stream's, and it says which.
    let raise = |name: String| {
        let js = js.clone();
        async move {
            let mut config = js.get_stream(&name).await.unwrap().get_info().await.unwrap().config;
            config.num_replicas = 3;
            js.update_stream(&config).await.unwrap_or_else(|e| panic!("raise {name}: {e}"));
            settled(&js, &name, 3).await;
        }
    };
    raise(format!("KV_{bucket}")).await;
    let err = match nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 3).await {
        Ok(_) => panic!("a stream with one copy was accepted as three"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains(&format!("stream `{wake_stream}`")), "names the stream: {err}");
    assert!(
        err.contains(&format!("nats stream edit {wake_stream} --replicas 3 --force")),
        "and the remedy: {err}"
    );
    raise(wake_stream.clone()).await;
    nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 3).await.unwrap();

    // More than was asked for is fine: the number is a floor, so a resumer left on the default
    // never lowers what `comp-park` asked for, and is never refused for it.
    nats::connect(&cluster.urls(), &bucket, &wake_stream).await.unwrap();
    nats::connect_with_replicas(&cluster.urls(), &bucket, &wake_stream, 2).await.unwrap();
    assert_eq!(placement(&js, &format!("KV_{bucket}")).await.unwrap().copies, 3, "left alone");
}

#[tokio::test]
async fn a_single_server_says_why_it_cannot_keep_three_copies() {
    let cluster = cluster_or_skip!("single", 1);
    let (bucket, wake_stream) = names();
    let url = cluster.urls();

    let err = match nats::connect_with_replicas(&url, &bucket, &wake_stream, 3).await {
        Ok(_) => panic!("one server cannot keep three copies"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains(&bucket) && err.contains("3 copies"), "names what was asked: {err}");
    assert!(err.contains("non-clustered"), "and the server's own reason: {err}");
    // Nothing was left half made.
    nats::connect(&url, &bucket, &wake_stream).await.expect("the default still works");

    // Not a number a stream can have: refused before any server is asked.
    for n in [0, 6, 100] {
        let e = nats::connect_with_replicas("nats://127.0.0.1:1", &bucket, &wake_stream, n)
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(e, ParkError::Invalid(_)), "{n}: {e}");
    }
    // A list with a hole in it is a typo, not a shorter list.
    let e = nats::connect(&format!("{url},,{url}"), &bucket, &wake_stream)
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(e, ParkError::Invalid(_)), "{e}");

    // A server that is down is passed over, which is what listing more than one is for.
    let dead = format!("nats://127.0.0.1:{}", free_port());
    nats::connect(&format!("{dead},{url}"), &bucket, &wake_stream)
        .await
        .expect("the first address is dead and the second is not");
}
