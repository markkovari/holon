//! The same behavior as `tests/mem.rs`, over a real NATS JetStream — plus
//! `WakeStream`'s own round trip, which nothing in-memory can prove.
//!
//! Gated on the environment, and LOUD when it skips:
//!
//!   HOLON_PARK_NATS_URL=nats://127.0.0.1:4333 \
//!   cargo test --manifest-path crates/Cargo.toml --features holon-park/native --test live
//!
//! `nats-server -js -p 4333 -sd <tmpdir>` is enough — no SurrealDB, no
//! compose: this crate's only dependency is JetStream.
#![cfg(feature = "native")]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use holon_park::engine::Engine;
use holon_park::model::{
    Agent, CallResult, OutboundCall, ParkOutcome, ReplayPolicy, Resolution, TicketId, TurnStatus,
    WakeKind, WakeMessage,
};
use holon_park::nats::{self, NatsParkStore, WakeStream};
use holon_park::recover::{RecoveryAction, RecoveryPolicy};
use holon_park::resume::{run, Delivery, Event, RunConfig, Sink, SinkReply};

fn agent(id: &str) -> Agent {
    Agent::named(id)
}

fn call(correlation: &str) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: "live test call".into(),
        deadline: None,
        poll: None,
        replay: ReplayPolicy::Unsafe,
        parent: None,
        group: None,
    }
}

fn result(ok: bool, body: &str) -> CallResult {
    CallResult { ok, body: body.as_bytes().to_vec(), detail: None }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// This test's own bucket/stream prefix, so parallel `cargo test` threads (and
/// reruns against a persistent NATS) never see each other's tickets. The counter
/// is what makes it unique: two tests starting in the same millisecond used to
/// share a bucket, and then each other's correlations.
fn tag() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("live-{}-{}-{}", std::process::id(), now_ms(), N.fetch_add(1, Ordering::SeqCst))
}

async fn make(name: &str) -> Option<Engine<NatsParkStore>> {
    let Ok(url) = std::env::var("HOLON_PARK_NATS_URL") else {
        eprintln!(
            "SKIPPED {name}: set HOLON_PARK_NATS_URL to run it against a real NATS JetStream"
        );
        return None;
    };
    let client = async_nats::connect(&url).await.expect("connect");
    let js = async_nats::jetstream::new(client);
    let bucket = format!("holon-park-test-{}", tag());
    let store = NatsParkStore::open(&js, &bucket).await.expect("open bucket");
    Some(Engine::new(store))
}

#[tokio::test]
async fn park_then_wake_then_take_ready_over_real_nats() {
    let Some(e) = make("park_then_wake_then_take_ready_over_real_nats").await else { return };
    let session = format!("s-{}", tag());

    let p = e.park(&session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    assert_eq!(p.outcome, ParkOutcome::Parked);

    let woken = e.wake("req-1", result(true, "42"), now_ms()).await.unwrap();
    assert_eq!(woken.ticket, p.ticket);
    assert!(woken.freshly_woken);

    let Resolution::Answered(r) = e.take_ready(&p.ticket, now_ms()).await.unwrap() else {
        panic!("expected an answer")
    };
    assert!(r.ok);
    assert_eq!(r.body, b"42");

    let err = e.take_ready(&p.ticket, now_ms()).await.unwrap_err();
    assert!(matches!(err, holon_park::ParkError::AlreadyClosed(_)), "{err:?}");
}

#[tokio::test]
async fn reparking_the_identical_call_is_idempotent_over_real_nats() {
    let Some(e) = make("reparking_the_identical_call_is_idempotent_over_real_nats").await else {
        return;
    };
    let session = format!("s-{}", tag());

    let first = e.park(&session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    let second = e.park(&session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    assert_eq!(second.ticket, first.ticket);
    assert_eq!(second.outcome, ParkOutcome::AlreadyParked);

    let pending = e.pending(&session, now_ms()).await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
}

#[tokio::test]
async fn oplog_lists_every_ticket_a_session_ever_parked_over_real_nats() {
    let Some(e) = make("oplog_lists_every_ticket_a_session_ever_parked_over_real_nats").await
    else {
        return;
    };
    let session = format!("s-{}", tag());

    let a = e.park(&session, call("req-a"), agent("a1"), now_ms()).await.unwrap();
    let b = e.park(&session, call("req-b"), agent("a1"), now_ms()).await.unwrap();
    e.cancel(&a.ticket, agent("a1"), now_ms()).await.unwrap();

    let log = e.oplog(&session, None, 10, now_ms()).await.unwrap();
    let tickets: std::collections::HashSet<_> = log.iter().map(|t| t.ticket.clone()).collect();
    assert!(tickets.contains(&a.ticket) && tickets.contains(&b.ticket), "{log:?}");
    let a_entry = log.iter().find(|t| t.ticket == a.ticket).unwrap();
    assert_eq!(a_entry.status, TurnStatus::Cancelled);
}

/// The one thing nothing in-memory can prove: a real `WorkQueue` stream — a
/// publish lands, a durable pull consumer fetches it, and acking it removes
/// it for good, the same lifecycle `comp-media`'s `MEDIA_JOBS` runs at real
/// load.
#[tokio::test]
async fn wake_stream_round_trips_through_a_durable_pull_consumer() {
    let Ok(url) = std::env::var("HOLON_PARK_NATS_URL") else {
        eprintln!("SKIPPED wake_stream_round_trips_through_a_durable_pull_consumer: set HOLON_PARK_NATS_URL");
        return;
    };
    let client = async_nats::connect(&url).await.expect("connect");
    let js = async_nats::jetstream::new(client);
    let name = format!("PARK_WAKE_TEST_{}", tag().replace(['-', '.'], "_"));
    let wake = WakeStream::open(&js, &name).await.expect("open wake stream");

    let msg = WakeMessage::resolved("s1", "t1");
    wake.publish(&msg).await.expect("publish");

    let stream = js.get_stream(&name).await.expect("get stream");
    let consumer: async_nats::jetstream::consumer::PullConsumer = stream
        .get_or_create_consumer(
            "test-resumer",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("test-resumer".into()),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                filter_subject: format!("{name}.ticket"),
                ..Default::default()
            },
        )
        .await
        .expect("consumer");

    use futures::StreamExt;
    let mut batch = consumer.fetch().max_messages(1).messages().await.expect("fetch");
    let delivered = batch.next().await.expect("a message").expect("not an error");
    let got: WakeMessage = serde_json::from_slice(&delivered.payload).unwrap();
    assert_eq!(got.ticket, "t1");
    assert_eq!(got.session, "s1");
    // `ack()` alone is fire-and-forget; wait for the server to process it
    // before asserting the message is gone.
    delivered.double_ack().await.expect("ack");

    // WorkQueue retention: an acked message is gone.
    let info = stream.get_info().await.expect("stream info");
    assert_eq!(
        info.state.messages, 0,
        "the acked message must be removed under WorkQueue retention"
    );
}

// ---- ADR-0106: recovery and the resumer, over real JetStream -------------------

fn url() -> Option<String> {
    std::env::var("HOLON_PARK_NATS_URL").ok()
}

macro_rules! need_nats {
    ($name:literal) => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!(
                    "SKIPPED {}: set HOLON_PARK_NATS_URL to run it against a real NATS JetStream",
                    $name
                );
                return;
            }
        }
    };
}

struct Stack {
    engine: Arc<Engine<NatsParkStore>>,
    wake: Arc<WakeStream>,
    session: String,
    stream: String,
}

async fn stack(url: &str) -> Stack {
    let t = tag();
    let stream = format!("PARK_WAKE_{}", t.replace(['-', '.'], "_"));
    let (store, wake) =
        nats::connect(url, &format!("holon-park-test-{t}"), &stream).await.expect("connect");
    Stack {
        engine: Arc::new(Engine::new(store)),
        wake: Arc::new(wake),
        session: format!("s-{t}"),
        stream,
    }
}

fn call_with(correlation: &str, replay: ReplayPolicy, deadline: Option<u64>) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: format!("live {correlation}"),
        deadline,
        poll: None,
        replay,
        parent: None,
        group: None,
    }
}

/// What a sink saw, and what to answer next (then `Delivered`).
#[derive(Default)]
struct Rec {
    seen: Mutex<Vec<Delivery>>,
    script: Mutex<VecDeque<SinkReply>>,
}

impl Rec {
    fn seen(&self) -> Vec<Delivery> {
        self.seen.lock().unwrap().clone()
    }
}

impl Sink for Rec {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        self.seen.lock().unwrap().push(d.clone());
        self.script.lock().unwrap().pop_front().unwrap_or(SinkReply::Delivered)
    }
}

async fn until(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn status_of(s: &Stack, ticket: &TicketId) -> TurnStatus {
    let log = s.engine.oplog(&s.session, None, 100, now_ms()).await.unwrap();
    log.into_iter().find(|e| &e.ticket == ticket).expect("ticket in the oplog").status
}

fn fast_policy() -> RecoveryPolicy {
    RecoveryPolicy {
        deadline_grace_ms: 0,
        max_redispatches: 2,
        retry_after_ms: 400,
        reannounce_after_ms: 1_000,
    }
}

fn cfg(consumer: &str) -> RunConfig {
    RunConfig {
        consumer: consumer.into(),
        ack_wait: Duration::from_secs(2),
        concurrency: 2,
        sweep_every: Duration::from_millis(150),
        policy: fast_policy(),
        ..Default::default()
    }
}

/// `comp-park`'s `wake` handler, minus HTTP: move the ticket, then announce it.
async fn wake_and_announce(s: &Stack, correlation: &str, body: &str) {
    let w = s.engine.wake(correlation, result(true, body), now_ms()).await.unwrap();
    assert!(w.freshly_woken);
    s.wake.publish(&WakeMessage::resolved(&w.session, &w.ticket)).await.unwrap();
}

#[tokio::test]
async fn a_recovery_pass_over_real_nats_interrupts_an_unsafe_call_and_leaves_the_rest() {
    let url =
        need_nats!("a_recovery_pass_over_real_nats_interrupts_an_unsafe_call_and_leaves_the_rest");
    let s = stack(&url).await;
    let now = now_ms();
    let u = s
        .engine
        .park(&s.session, call_with("u", ReplayPolicy::Unsafe, Some(now - 1)), agent("a1"), now)
        .await
        .unwrap();
    let a = s
        .engine
        .park(&s.session, call_with("a", ReplayPolicy::Abandon, Some(now - 1)), agent("a1"), now)
        .await
        .unwrap();
    let later = s
        .engine
        .park(
            &s.session,
            call_with("later", ReplayPolicy::Unsafe, Some(now + 3_600_000)),
            agent("a1"),
            now,
        )
        .await
        .unwrap();
    let none = s
        .engine
        .park(&s.session, call_with("none", ReplayPolicy::Unsafe, None), agent("a1"), now)
        .await
        .unwrap();

    // The bucket is this test's own, so a scan finds exactly these four.
    let done = s.engine.recover(&agent("sweeper"), &fast_policy(), now_ms()).await.unwrap();
    let mut got: Vec<_> = done.iter().map(|d| (d.ticket.clone(), d.action.clone())).collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = vec![
        (u.ticket.clone(), RecoveryAction::Interrupted),
        (a.ticket.clone(), RecoveryAction::Abandoned),
    ];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, want);

    assert_eq!(status_of(&s, &u.ticket).await, TurnStatus::Interrupted);
    assert_eq!(status_of(&s, &a.ticket).await, TurnStatus::Cancelled);
    assert_eq!(status_of(&s, &later.ticket).await, TurnStatus::Parked);
    assert_eq!(status_of(&s, &none.ticket).await, TurnStatus::Parked);
    assert!(matches!(
        s.engine.take_ready(&u.ticket, now_ms()).await.unwrap(),
        Resolution::Interrupted(_)
    ));

    // A second pass has nothing left to do.
    assert!(s
        .engine
        .recover(&agent("sweeper"), &fast_policy(), now_ms())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn two_passes_at_once_over_real_nats_act_once_per_ticket() {
    let url = need_nats!("two_passes_at_once_over_real_nats_act_once_per_ticket");
    let s = stack(&url).await;
    let now = now_ms();
    for i in 0..12 {
        let replay = if i % 2 == 0 { ReplayPolicy::Unsafe } else { ReplayPolicy::Safe };
        s.engine
            .park(&s.session, call_with(&format!("c{i}"), replay, Some(now - 1)), agent("a1"), now)
            .await
            .unwrap();
    }
    let mut passes = Vec::new();
    for _ in 0..6 {
        let e = s.engine.clone();
        passes.push(tokio::spawn(async move {
            e.recover(&agent("sweeper"), &fast_policy(), now_ms()).await.unwrap()
        }));
    }
    let (mut interrupted, mut granted) = (0, 0);
    for p in passes {
        for r in p.await.unwrap() {
            match r.action {
                RecoveryAction::Interrupted => interrupted += 1,
                RecoveryAction::Redispatch { attempt: 1 } => granted += 1,
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!(
        (interrupted, granted),
        (6, 6),
        "each ticket acted on exactly once between six passes"
    );
}

#[tokio::test]
async fn announcing_one_thing_twice_queues_it_once() {
    let url = need_nats!("announcing_one_thing_twice_queues_it_once");
    let s = stack(&url).await;
    let js = async_nats::jetstream::new(async_nats::connect(&url).await.unwrap());
    let msg = WakeMessage::resolved("s1", "t1");

    s.wake.publish(&msg).await.unwrap();
    s.wake.publish(&msg).await.unwrap(); // a recovery pass and a wake, both announcing the same answer
    s.wake.publish(&WakeMessage::redispatch("s1", "t1", 1)).await.unwrap();
    s.wake.publish(&WakeMessage::redispatch("s1", "t1", 2)).await.unwrap();
    s.wake.publish(&WakeMessage::redispatch("s1", "t1", 2)).await.unwrap();

    let info = js.get_stream(&s.stream).await.unwrap().get_info().await.unwrap();
    assert_eq!(info.state.messages, 3, "resolved once, and each re-dispatch attempt once");
}

#[tokio::test]
async fn a_resumer_delivers_an_answer_and_closes_the_ticket() {
    let url = need_nats!("a_resumer_delivers_an_answer_and_closes_the_ticket");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    let p = s.engine.park(&s.session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    wake_and_announce(&s, "req-1", "42").await;

    until("the sink to be handed the answer", 20, || !sink.seen().is_empty()).await;
    let d = sink.seen()[0].clone();
    assert_eq!((d.ticket.as_str(), d.correlation.as_str()), (p.ticket.as_str(), "req-1"));
    assert_eq!(d.event, Event::Answered { answer: result(true, "42") });
    for _ in 0..100 {
        if status_of(&s, &p.ticket).await == TurnStatus::Resumed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(status_of(&s, &p.ticket).await, TurnStatus::Resumed);

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
    assert_eq!(sink.seen().len(), 1, "once, not once per announcement");
}

#[tokio::test]
async fn a_resumer_tells_the_turn_about_a_call_nobody_answered_and_retries_a_busy_sink() {
    let url =
        need_nats!("a_resumer_tells_the_turn_about_a_call_nobody_answered_and_retries_a_busy_sink");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    // The sink is busy for the first delivery: the message must come back.
    sink.script.lock().unwrap().push_back(SinkReply::Retry("busy".into()));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    let now = now_ms();
    let p = s
        .engine
        .park(
            &s.session,
            call_with("pay-1", ReplayPolicy::Unsafe, Some(now + 300)),
            agent("a1"),
            now,
        )
        .await
        .unwrap();

    until("the turn to be told twice (busy, then taken)", 30, || sink.seen().len() >= 2).await;
    let seen = sink.seen();
    assert!(matches!(seen[0].event, Event::Interrupted { .. }), "{:?}", seen[0]);
    assert_eq!(
        seen[0].idempotency_key(),
        seen[1].idempotency_key(),
        "a redelivery is the same delivery"
    );
    assert_eq!(seen[1].ticket, p.ticket);
    for _ in 0..100 {
        if status_of(&s, &p.ticket).await == TurnStatus::Resumed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(status_of(&s, &p.ticket).await, TurnStatus::Resumed);

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_safe_call_is_redispatched_twice_then_the_turn_is_told_it_was_interrupted() {
    let url =
        need_nats!("a_safe_call_is_redispatched_twice_then_the_turn_is_told_it_was_interrupted");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    let now = now_ms();
    s.engine
        .park(
            &s.session,
            call_with("read-1", ReplayPolicy::Safe, Some(now + 200)),
            agent("a1"),
            now,
        )
        .await
        .unwrap();

    until("two re-dispatches and an interruption", 30, || sink.seen().len() >= 3).await;
    let kinds: Vec<String> = sink
        .seen()
        .iter()
        .map(|d| match &d.event {
            Event::Redispatch { attempt, description } => {
                format!("redispatch {attempt} {description}")
            }
            Event::Interrupted { interruption } => {
                format!("interrupted: {}", interruption.reason.contains("2 re-dispatches"))
            }
            Event::Answered { .. } => "answered".into(),
            Event::Joined { .. } => "joined".into(),
        })
        .collect();
    assert_eq!(
        kinds,
        ["redispatch 1 live read-1", "redispatch 2 live read-1", "interrupted: true"]
    );

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

/// The resumer is killed after its sink was handed the answer and before it
/// closed the ticket. A second resumer picks the message up when the server
/// gives up waiting for the first, and the sink is handed the same delivery.
#[tokio::test]
async fn a_resumer_killed_mid_delivery_is_replaced_without_losing_the_answer() {
    let url = need_nats!("a_resumer_killed_mid_delivery_is_replaced_without_losing_the_answer");
    let s = stack(&url).await;

    /// Accepts the delivery in its own words only once released; until then it
    /// says nothing, which is what a sink looks like to a resumer that dies.
    struct Hangs(Mutex<Vec<Delivery>>);
    impl Sink for Hangs {
        async fn deliver(&self, d: &Delivery) -> SinkReply {
            self.0.lock().unwrap().push(d.clone());
            std::future::pending::<()>().await;
            SinkReply::Delivered
        }
    }
    let hung = Arc::new(Hangs(Mutex::new(Vec::new())));

    let p = s.engine.park(&s.session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    wake_and_announce(&s, "req-1", "42").await;

    // Resumer one lives on a runtime of its own, so it can be killed outright.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_stop, rx) = tokio::sync::watch::channel(false);
    rt.spawn(run(s.engine.clone(), s.wake.clone(), hung.clone(), cfg("t-resumer"), rx));
    until("resumer one to be mid-delivery", 20, || !hung.0.lock().unwrap().is_empty()).await;
    rt.shutdown_background(); // every task, and its NATS connection, gone at once
    assert_eq!(status_of(&s, &p.ticket).await, TurnStatus::Ready, "nothing was closed");

    // Resumer two: same consumer name, a sink that answers.
    let sink = Arc::new(Rec::default());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    until("the answer to reach the replacement", 30, || !sink.seen().is_empty()).await;
    assert_eq!(sink.seen()[0].idempotency_key(), hung.0.lock().unwrap()[0].idempotency_key());
    assert_eq!(sink.seen()[0].event, Event::Answered { answer: result(true, "42") });
    for _ in 0..100 {
        if status_of(&s, &p.ticket).await == TurnStatus::Resumed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(status_of(&s, &p.ticket).await, TurnStatus::Resumed);

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

/// `PARK_WAKE` loses a nudge (here: it is never published). The ticket is
/// `ready` and nobody was told; the recovery pass finds it.
#[tokio::test]
async fn an_answer_whose_announcement_was_lost_is_found_by_the_sweep() {
    let url = need_nats!("an_answer_whose_announcement_was_lost_is_found_by_the_sweep");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    let p = s.engine.park(&s.session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    // `comp-park`'s wake, with its publish failing: the record is durable, the nudge is not.
    s.engine.wake("req-1", result(true, "42"), now_ms()).await.unwrap();

    until("the sweep to re-announce it (reannounce_after_ms = 1s)", 30, || !sink.seen().is_empty())
        .await;
    assert_eq!(sink.seen()[0].ticket, p.ticket);
    assert_eq!(sink.seen()[0].event, Event::Answered { answer: result(true, "42") });

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_message_published_by_the_old_comp_park_is_still_understood() {
    let url = need_nats!("a_message_published_by_the_old_comp_park_is_still_understood");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    let p = s.engine.park(&s.session, call("req-1"), agent("a1"), now_ms()).await.unwrap();
    s.engine.wake("req-1", result(true, "42"), now_ms()).await.unwrap();

    // Before ADR-0106 a wake message was `{session, ticket}` and nothing else.
    let js = async_nats::jetstream::new(async_nats::connect(&url).await.unwrap());
    let old = serde_json::json!({"session": s.session, "ticket": p.ticket}).to_string();
    js.publish(format!("{}.ticket", s.stream), old.into()).await.unwrap().await.unwrap();

    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));
    until("the old-format message to be delivered", 20, || !sink.seen().is_empty()).await;
    assert!(matches!(sink.seen()[0].event, Event::Answered { .. }));
    assert_eq!(WakeKind::default(), WakeKind::Resolved);

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

// ---- ADR-0109: groups over real JetStream ------------------------------------------------------

fn group_of(
    correlation: &str,
    policy: holon_park::model::JoinPolicy,
    children: u32,
) -> OutboundCall {
    OutboundCall {
        group: Some(holon_park::model::GroupSpec { policy, children }),
        ..call_with(correlation, ReplayPolicy::Unsafe, None)
    }
}

fn child_of(correlation: &str, parent: &str) -> OutboundCall {
    OutboundCall {
        parent: Some(parent.into()),
        ..call_with(correlation, ReplayPolicy::Unsafe, None)
    }
}

/// A fan-out, answered by the far side in any order: the group resolves once, the resumer
/// delivers one `joined` event and no child on its own, and everything is closed.
#[tokio::test]
async fn a_fan_out_over_real_nats_resolves_once_and_is_delivered_as_one_joined_event() {
    let url =
        need_nats!("a_fan_out_over_real_nats_resolves_once_and_is_delivered_as_one_joined_event");
    let s = stack(&url).await;
    let sink = Arc::new(Rec::default());
    let (stop, rx) = tokio::sync::watch::channel(false);
    let r = tokio::spawn(run(s.engine.clone(), s.wake.clone(), sink.clone(), cfg("t-resumer"), rx));

    let now = now_ms();
    let g = s
        .engine
        .park(
            &s.session,
            group_of("g", holon_park::model::JoinPolicy::AllSettled, 3),
            agent("a1"),
            now,
        )
        .await
        .unwrap();
    for c in ["c1", "c2", "c3"] {
        s.engine.park(&s.session, child_of(c, "g"), agent("a1"), now).await.unwrap();
    }

    // What `comp-park` does for each answer: move the child, and publish what the engine says.
    for (i, c) in ["c2", "c3", "c1"].iter().enumerate() {
        let w = s.engine.wake(c, result(true, c), now_ms()).await.unwrap();
        assert_eq!(
            w.announce.len(),
            usize::from(i == 2),
            "only the last answer resolves it: {w:?}"
        );
        for m in &w.announce {
            s.wake.publish(m).await.unwrap();
        }
    }

    until("the group to be delivered", 20, || !sink.seen().is_empty()).await;
    let d = sink.seen()[0].clone();
    assert_eq!(d.ticket, g.ticket);
    let Event::Joined { joined } = &d.event else { panic!("{d:?}") };
    assert_eq!(joined.outcome, holon_park::model::JoinOutcome::AllAnswered);
    let order: Vec<_> = joined.children.iter().map(|c| c.correlation.as_str()).collect();
    assert_eq!(
        order,
        ["c1", "c2", "c3"],
        "in the order they were parked, not the order they answered"
    );

    for _ in 0..100 {
        if status_of(&s, &g.ticket).await == TurnStatus::Resumed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(status_of(&s, &g.ticket).await, TurnStatus::Resumed);
    let log = s.engine.oplog(&s.session, None, 100, now_ms()).await.unwrap();
    assert!(log.iter().filter(|e| e.parent.is_some()).all(|e| e.status == TurnStatus::Resumed));
    assert_eq!(sink.seen().len(), 1, "one delivery for the whole fan-out");

    stop.send(true).unwrap();
    r.await.unwrap().unwrap();
}

/// The bound on a group's list is checked and written in one compare-and-set, so a fan-out that
/// registers its children all at once cannot take more places than it declared — and the racers
/// the contention turns away can retry, because registering is idempotent.
#[tokio::test]
async fn a_fan_out_racing_for_a_group_over_real_nats_cannot_exceed_it() {
    let url = need_nats!("a_fan_out_racing_for_a_group_over_real_nats_cannot_exceed_it");
    let s = stack(&url).await;
    let now = now_ms();
    s.engine
        .park(
            &s.session,
            group_of("g", holon_park::model::JoinPolicy::AllSettled, 5),
            agent("a1"),
            now,
        )
        .await
        .unwrap();

    let mut racers = Vec::new();
    for i in 0..24 {
        let (e, session) = (s.engine.clone(), s.session.clone());
        racers.push(tokio::spawn(async move {
            // A racer the contention turned away tries again, as a caller would.
            for _ in 0..20 {
                match e.park(&session, child_of(&format!("c{i}"), "g"), agent("a1"), now_ms()).await
                {
                    Ok(_) => return true,
                    Err(holon_park::ParkError::Invalid(m)) if m.contains("full") => return false,
                    Err(holon_park::ParkError::Storage(_)) => {
                        tokio::time::sleep(Duration::from_millis(5)).await
                    }
                    Err(e) => panic!("{e:?}"),
                }
            }
            panic!("never got an answer");
        }));
    }
    let mut taken = 0;
    for r in racers {
        taken += usize::from(r.await.unwrap());
    }
    assert_eq!(taken, 5, "exactly the places the group was declared with");
}

#[tokio::test]
async fn a_fail_fast_group_over_real_nats_cancels_the_siblings_and_resolves() {
    let url = need_nats!("a_fail_fast_group_over_real_nats_cancels_the_siblings_and_resolves");
    let s = stack(&url).await;
    let now = now_ms();
    s.engine
        .park(
            &s.session,
            group_of("g", holon_park::model::JoinPolicy::FailFast, 3),
            agent("a1"),
            now,
        )
        .await
        .unwrap();
    for c in ["c1", "c2", "c3"] {
        s.engine.park(&s.session, child_of(c, "g"), agent("a1"), now).await.unwrap();
    }
    s.engine.wake("c1", result(true, "ok"), now_ms()).await.unwrap();

    let g = ticket_of(&s, "g");
    let out =
        s.engine.interrupt(&ticket_of(&s, "c2"), "boom", agent("a1"), now_ms()).await.unwrap();
    assert_eq!(out.announce, [WakeMessage::resolved(&s.session, &g)]);

    let Resolution::Joined(j) = s.engine.take_ready(&g, now_ms()).await.unwrap() else {
        panic!("a group resolves as joined")
    };
    assert_eq!(j.outcome, holon_park::model::JoinOutcome::FailedFast);
    let ended: Vec<_> = j
        .children
        .iter()
        .map(|c| format!("{:?}", c.ended).chars().take(11).collect::<String>())
        .collect();
    assert_eq!(ended, ["Answered(Ca", "Interrupted", "Cancelled"]);
}

fn ticket_of(s: &Stack, correlation: &str) -> TicketId {
    holon_park::ticket::ticket_id(&s.session, correlation)
}
