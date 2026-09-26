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

use std::time::{SystemTime, UNIX_EPOCH};

use holon_park::engine::Engine;
use holon_park::model::{Agent, CallResult, OutboundCall, ParkOutcome, TurnStatus};
use holon_park::nats::{NatsParkStore, WakeMessage, WakeStream};

fn agent(id: &str) -> Agent {
    Agent::named(id)
}

fn call(correlation: &str) -> OutboundCall {
    OutboundCall { correlation: correlation.into(), description: "live test call".into(), deadline: None, poll: None }
}

fn result(ok: bool, body: &str) -> CallResult {
    CallResult { ok, body: body.as_bytes().to_vec(), detail: None }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// This run's own bucket/stream prefix, so parallel `cargo test` runs (and
/// reruns against a persistent NATS) never see each other's tickets.
fn tag() -> String {
    format!("live-{}-{}", std::process::id(), now_ms())
}

async fn make(name: &str) -> Option<Engine<NatsParkStore>> {
    let Ok(url) = std::env::var("HOLON_PARK_NATS_URL") else {
        eprintln!("SKIPPED {name}: set HOLON_PARK_NATS_URL to run it against a real NATS JetStream");
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

    let r = e.take_ready(&p.ticket, now_ms()).await.unwrap();
    assert!(r.ok);
    assert_eq!(r.body, b"42");

    let err = e.take_ready(&p.ticket, now_ms()).await.unwrap_err();
    assert!(matches!(err, holon_park::ParkError::AlreadyClosed(_)), "{err:?}");
}

#[tokio::test]
async fn reparking_the_identical_call_is_idempotent_over_real_nats() {
    let Some(e) = make("reparking_the_identical_call_is_idempotent_over_real_nats").await else { return };
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
    let Some(e) = make("oplog_lists_every_ticket_a_session_ever_parked_over_real_nats").await else { return };
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

    let msg = WakeMessage { session: "s1".into(), ticket: "t1".into() };
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
    // `ack()` alone is a fire-and-forget publish of the ack reply — this test
    // needs to know the server actually processed it before asserting the
    // message is gone, so it waits for the round trip `double_ack` gives.
    delivered.double_ack().await.expect("ack");

    // WorkQueue retention: an acked message is gone. A second fetch on a
    // fresh ephemeral consumer over the same subject sees nothing.
    let info = stream.get_info().await.expect("stream info");
    assert_eq!(info.state.messages, 0, "the acked message must be removed under WorkQueue retention");
}
