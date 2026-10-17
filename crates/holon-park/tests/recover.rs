//! The recovery pass and the resumer's delivery (ADR-0106), against the
//! in-memory store: the table in `recover.rs` as a table, and the
//! at-least-once promise `resume::handle` makes to a sink.
//!
//! Time is injected everywhere, so nothing here sleeps.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use holon_park::error::{ParkError, Result};
use holon_park::mem::MemParkStore;
use holon_park::model::{
    Agent, CallResult, OutboundCall, ReplayPolicy, Resolution, TicketId, TurnStatus, WakeKind,
    WakeMessage,
};
use holon_park::recover::{RecoveryAction, RecoveryPolicy, Repaired};
use holon_park::resume::{handle, sweep, Announcer, Delivery, Event, Handled, Sink, SinkReply};
use holon_park::store::{ParkStore, Record, Revision};
use holon_park::ticket::ticket_id;
use holon_park::{mem_engine, Engine};

const S: &str = "s1";

fn sweeper() -> Agent {
    Agent::named("comp-resume")
}

fn policy() -> RecoveryPolicy {
    RecoveryPolicy {
        deadline_grace_ms: 100,
        max_redispatches: 3,
        retry_after_ms: 1_000,
        reannounce_after_ms: 500,
    }
}

fn call(correlation: &str, replay: ReplayPolicy, deadline: Option<u64>) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: format!("call {correlation}"),
        deadline,
        poll: None,
        replay,
    }
}

fn answer(body: &str) -> CallResult {
    CallResult { ok: true, body: body.as_bytes().to_vec(), detail: None }
}

async fn park<St: ParkStore>(
    e: &Engine<St>,
    correlation: &str,
    replay: ReplayPolicy,
    deadline: Option<u64>,
) -> TicketId {
    e.park(&S.to_string(), call(correlation, replay, deadline), Agent::named("turn"), 1_000)
        .await
        .unwrap()
        .ticket
}

async fn status<St: ParkStore>(e: &Engine<St>, now: u64) -> Vec<TurnStatus> {
    e.oplog(&S.to_string(), None, 100, now).await.unwrap().into_iter().map(|t| t.status).collect()
}

// ---- the table ---------------------------------------------------------------

#[tokio::test]
async fn an_unsafe_call_past_its_deadline_is_interrupted_and_the_turn_is_told_why() {
    let e = mem_engine();
    let t = park(&e, "pay-1", ReplayPolicy::Unsafe, Some(5_000)).await;

    let done = e.recover(&sweeper(), &policy(), 6_000).await.unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!((done[0].ticket.as_str(), done[0].correlation.as_str()), (t.as_str(), "pay-1"));
    assert_eq!(done[0].action, RecoveryAction::Interrupted);
    assert_eq!(
        done[0].announcement(),
        Some(WakeMessage::resolved(S, &t)),
        "an interruption is announced as something to take"
    );

    match e.take_ready(&t, 6_100).await.unwrap() {
        Resolution::Interrupted(i) => {
            assert_eq!((i.at, i.by.id.as_str()), (6_000, "comp-resume"));
            assert!(
                i.reason.contains("not replay-safe") && i.reason.contains("Check on the far side"),
                "the model is told what to do about it: {}",
                i.reason
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_call_inside_its_deadline_or_its_grace_is_left_alone() {
    let e = mem_engine();
    park(&e, "c", ReplayPolicy::Unsafe, Some(5_000)).await;

    for now in [0, 4_999, 5_000, 5_099] {
        assert!(e.recover(&sweeper(), &policy(), now).await.unwrap().is_empty(), "at {now}");
    }
    assert_eq!(status(&e, 5_099).await, [TurnStatus::Expired], "expired is a read, not an act");
    assert_eq!(e.recover(&sweeper(), &policy(), 5_100).await.unwrap().len(), 1, "grace over");
}

#[tokio::test]
async fn a_call_with_no_deadline_is_never_given_up_on() {
    let e = mem_engine();
    for (i, replay) in
        [ReplayPolicy::Safe, ReplayPolicy::Unsafe, ReplayPolicy::Abandon].into_iter().enumerate()
    {
        park(&e, &format!("c{i}"), replay, None).await;
    }
    assert!(e.recover(&sweeper(), &policy(), u64::MAX).await.unwrap().is_empty());
    assert_eq!(status(&e, 1).await, [TurnStatus::Parked; 3]);
}

#[tokio::test]
async fn an_abandon_call_past_its_deadline_is_cancelled_and_nobody_is_told() {
    let e = mem_engine();
    let t = park(&e, "ping", ReplayPolicy::Abandon, Some(5_000)).await;

    let done = e.recover(&sweeper(), &policy(), 6_000).await.unwrap();
    assert_eq!(done[0].action, RecoveryAction::Abandoned);
    assert_eq!(done[0].announcement(), None);
    assert_eq!(status(&e, 6_000).await, [TurnStatus::Cancelled]);
    assert!(e.pending(&S.to_string(), 6_000).await.unwrap().is_empty());
    assert!(matches!(e.take_ready(&t, 6_001).await.unwrap_err(), ParkError::AlreadyClosed(_)));
}

#[tokio::test]
async fn a_safe_call_is_redispatched_a_bounded_number_of_times_then_interrupted() {
    let e = mem_engine();
    let t = park(&e, "read-1", ReplayPolicy::Safe, Some(5_000)).await;
    let p = policy();

    // Each grant pushes the deadline out by `retry_after_ms`, so the next one is
    // due `retry_after_ms + grace` later and not before.
    let mut now = 5_100;
    for attempt in 1..=3 {
        let done = e.recover(&sweeper(), &p, now).await.unwrap();
        assert_eq!(done[0].action, RecoveryAction::Redispatch { attempt }, "at {now}");
        assert_eq!(done[0].announcement(), Some(WakeMessage::redispatch(S, &t, attempt)));

        let entry = &e.oplog(&S.to_string(), None, 1, now).await.unwrap()[0];
        assert_eq!(entry.status, TurnStatus::Parked, "a re-dispatch is not a state change");
        assert_eq!((entry.redispatches, entry.deadline), (attempt, Some(now + 1_000)));

        assert!(
            e.recover(&sweeper(), &p, now + 1_099).await.unwrap().is_empty(),
            "not again until the new deadline and its grace have passed"
        );
        now += 1_100;
    }

    // Three granted: the fourth time round it stops asking and says so.
    let done = e.recover(&sweeper(), &p, now).await.unwrap();
    assert_eq!(done[0].action, RecoveryAction::Interrupted);
    match e.take_ready(&t, now + 1).await.unwrap() {
        Resolution::Interrupted(i) => assert!(i.reason.contains("3 re-dispatches"), "{}", i.reason),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_answer_ends_a_safe_call_before_the_next_redispatch() {
    let e = mem_engine();
    let t = park(&e, "read-1", ReplayPolicy::Safe, Some(5_000)).await;
    e.recover(&sweeper(), &policy(), 5_100).await.unwrap();
    e.wake("read-1", answer("42"), 5_200).await.unwrap();

    // Ready now; far past the pushed-out deadline, and nothing happens to it
    // until it has sat untaken long enough to be announced again.
    let done = e.recover(&sweeper(), &policy(), 6_300).await.unwrap();
    assert_eq!(
        done.iter().map(|d| d.action.clone()).collect::<Vec<_>>(),
        [RecoveryAction::Reannounce]
    );
    match e.take_ready(&t, 6_400).await.unwrap() {
        Resolution::Answered(a) => assert_eq!(a.body, b"42"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_pass_run_twice_at_the_same_instant_acts_once() {
    let e = mem_engine();
    park(&e, "a", ReplayPolicy::Unsafe, Some(5_000)).await;
    park(&e, "b", ReplayPolicy::Abandon, Some(5_000)).await;
    park(&e, "c", ReplayPolicy::Safe, Some(5_000)).await;

    assert_eq!(e.recover(&sweeper(), &policy(), 6_000).await.unwrap().len(), 3);
    assert!(e.recover(&sweeper(), &policy(), 6_000).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_untaken_answer_is_announced_again_but_not_on_every_pass() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, None).await;
    e.wake("c", answer("x"), 2_000).await.unwrap();
    let p = policy(); // reannounce_after_ms = 500

    assert!(e.recover(&sweeper(), &p, 2_499).await.unwrap().is_empty(), "too soon");
    let done = e.recover(&sweeper(), &p, 2_500).await.unwrap();
    assert_eq!(done[0].action, RecoveryAction::Reannounce);
    assert_eq!(done[0].announcement(), Some(WakeMessage::resolved(S, &t)));

    // A sink that is down must not be re-announced to every pass.
    assert!(e.recover(&sweeper(), &p, 2_999).await.unwrap().is_empty());
    assert_eq!(e.recover(&sweeper(), &p, 3_000).await.unwrap().len(), 1, "once per gap");

    // And what was re-announced is still the same, untaken, answer.
    assert!(matches!(e.take_ready(&t, 3_100).await.unwrap(), Resolution::Answered(_)));
    assert!(e.recover(&sweeper(), &p, 99_999).await.unwrap().is_empty(), "taken: finished");
}

#[tokio::test]
async fn an_untaken_interruption_is_announced_again_too() {
    // A pass that died between moving the ticket and announcing it leaves
    // exactly this, and this is what finds it.
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, Some(1_000)).await;
    e.interrupt(&t, "gave up", sweeper(), 2_000).await.unwrap();

    let done = e.recover(&sweeper(), &policy(), 2_500).await.unwrap();
    assert_eq!(done[0].action, RecoveryAction::Reannounce);
}

#[tokio::test]
async fn finished_tickets_are_left_alone() {
    let e = mem_engine();
    let a = park(&e, "a", ReplayPolicy::Unsafe, Some(1_000)).await;
    let b = park(&e, "b", ReplayPolicy::Unsafe, Some(1_000)).await;
    e.wake("a", answer("x"), 1_100).await.unwrap();
    e.take_ready(&a, 1_200).await.unwrap();
    e.cancel(&b, Agent::named("turn"), 1_300).await.unwrap();

    assert!(e.recover(&sweeper(), &policy(), u64::MAX).await.unwrap().is_empty());
}

/// The late answer of an interrupted call is dropped, and the pass that
/// interrupted it is what decided the answer was late.
#[tokio::test]
async fn an_answer_arriving_after_recovery_interrupted_the_call_is_dropped() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, Some(1_000)).await;
    e.recover(&sweeper(), &policy(), 2_000).await.unwrap();

    let woken = e.wake("c", answer("late"), 2_100).await.unwrap();
    assert!(!woken.freshly_woken);
    assert!(matches!(e.take_ready(&t, 2_200).await.unwrap(), Resolution::Interrupted(_)));
}

/// Several resumers sweeping the same store at the same instant: every
/// interruption and every grant happens exactly once between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn racing_passes_act_on_each_ticket_exactly_once() {
    let e = Arc::new(mem_engine());
    for i in 0..20 {
        let replay = if i % 2 == 0 { ReplayPolicy::Unsafe } else { ReplayPolicy::Safe };
        park(&e, &format!("c{i}"), replay, Some(1_000)).await;
    }

    let mut handles = Vec::new();
    for _ in 0..8 {
        let e = e.clone();
        handles.push(tokio::spawn(async move {
            e.recover(&sweeper(), &policy(), 5_000).await.unwrap()
        }));
    }
    let mut interrupted = 0;
    let mut granted = 0;
    for h in handles {
        for r in h.await.unwrap() {
            match r.action {
                RecoveryAction::Interrupted => interrupted += 1,
                RecoveryAction::Redispatch { attempt } => {
                    assert_eq!(attempt, 1, "never granted twice");
                    granted += 1;
                }
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!((interrupted, granted), (10, 10));
}

// ---- the sweep: recover, then say so -------------------------------------------

#[derive(Default)]
struct Said {
    sent: Mutex<Vec<WakeMessage>>,
    broken: AtomicBool,
}

impl Announcer for Said {
    async fn announce(&self, msg: &WakeMessage) -> Result<()> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(ParkError::storage("PARK_WAKE is down"));
        }
        self.sent.lock().unwrap().push(msg.clone());
        Ok(())
    }
}

#[tokio::test]
async fn a_sweep_announces_what_it_moved_and_only_that() {
    let e = mem_engine();
    let unsafe_t = park(&e, "u", ReplayPolicy::Unsafe, Some(1_000)).await;
    let safe_t = park(&e, "s", ReplayPolicy::Safe, Some(1_000)).await;
    park(&e, "a", ReplayPolicy::Abandon, Some(1_000)).await;
    park(&e, "later", ReplayPolicy::Unsafe, Some(900_000)).await;
    let said = Said::default();

    let swept = sweep(&e, &said, &sweeper(), &policy(), 2_000).await.unwrap();
    assert_eq!(swept.recovered.len(), 3);
    assert!(swept.unannounced.is_empty());

    let mut sent = said.sent.lock().unwrap().clone();
    sent.sort_by(|a, b| a.ticket.cmp(&b.ticket));
    let mut want =
        vec![WakeMessage::resolved(S, &unsafe_t), WakeMessage::redispatch(S, &safe_t, 1)];
    want.sort_by(|a, b| a.ticket.cmp(&b.ticket));
    assert_eq!(sent, want, "the abandoned call and the one not yet due say nothing");
}

/// The announcement is not the durability boundary: if it fails, the ticket has
/// still moved, and a later pass finds it.
#[tokio::test]
async fn a_sweep_whose_announcement_fails_still_moved_the_ticket() {
    let e = mem_engine();
    let t = park(&e, "u", ReplayPolicy::Unsafe, Some(1_000)).await;
    let said = Said::default();
    said.broken.store(true, Ordering::SeqCst);

    let swept = sweep(&e, &said, &sweeper(), &policy(), 2_000).await.unwrap();
    assert_eq!(swept.recovered.len(), 1);
    assert_eq!(swept.unannounced.len(), 1);
    assert_eq!(swept.unannounced[0].0, t);
    assert_eq!(status(&e, 2_000).await, [TurnStatus::Interrupted]);

    // PARK_WAKE comes back; the next pass past the grace announces it.
    said.broken.store(false, Ordering::SeqCst);
    let swept = sweep(&e, &said, &sweeper(), &policy(), 2_600).await.unwrap();
    assert_eq!(swept.recovered[0].action, RecoveryAction::Reannounce);
    assert_eq!(*said.sent.lock().unwrap(), [WakeMessage::resolved(S, &t)]);
}

// ---- delivery ------------------------------------------------------------------

/// A sink that records what it is handed and answers from a script (then
/// `Delivered`).
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<Delivery>>,
    script: Mutex<VecDeque<SinkReply>>,
}

impl Recorder {
    fn replying(replies: impl IntoIterator<Item = SinkReply>) -> Self {
        Recorder { script: Mutex::new(replies.into_iter().collect()), ..Default::default() }
    }
    fn seen(&self) -> Vec<Delivery> {
        self.seen.lock().unwrap().clone()
    }
}

impl Sink for Recorder {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        self.seen.lock().unwrap().push(d.clone());
        self.script.lock().unwrap().pop_front().unwrap_or(SinkReply::Delivered)
    }
}

async fn run<St: ParkStore, K: Sink>(
    e: &Engine<St>,
    sink: &K,
    msg: &WakeMessage,
    now: u64,
) -> Result<Handled> {
    handle(e, sink, msg, &sweeper(), now).await
}

#[tokio::test]
async fn an_answer_is_delivered_and_only_then_the_ticket_is_closed() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, None).await;
    e.wake("c", answer("42"), 2_000).await.unwrap();
    let sink = Recorder::default();

    let got = run(&e, &sink, &WakeMessage::resolved(S, &t), 2_100).await.unwrap();
    assert_eq!(got, Handled::Delivered);

    let seen = sink.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].session.as_str(), seen[0].correlation.as_str()), (S, "c"));
    assert_eq!(seen[0].event, Event::Answered { answer: answer("42") });
    assert_eq!(status(&e, 2_200).await, [TurnStatus::Resumed]);

    // The same message again (a duplicate nudge): nothing to do, nobody bothered.
    assert_eq!(run(&e, &sink, &WakeMessage::resolved(S, &t), 2_300).await.unwrap(), Handled::Stale);
    assert_eq!(sink.seen().len(), 1);
}

#[tokio::test]
async fn an_interruption_is_delivered_as_one() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, Some(1_000)).await;
    e.recover(&sweeper(), &policy(), 2_000).await.unwrap();
    let sink = Recorder::default();

    run(&e, &sink, &WakeMessage::resolved(S, &t), 2_100).await.unwrap();
    match &sink.seen()[0].event {
        Event::Interrupted { interruption } => assert_eq!(interruption.at, 2_000),
        other => panic!("{other:?}"),
    }
    assert_eq!(status(&e, 2_200).await, [TurnStatus::Resumed]);
}

#[tokio::test]
async fn a_sink_that_says_not_now_leaves_the_answer_where_it_was() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, None).await;
    e.wake("c", answer("42"), 2_000).await.unwrap();
    let sink =
        Recorder::replying([SinkReply::Retry("busy".into()), SinkReply::Retry("busy".into())]);
    let msg = WakeMessage::resolved(S, &t);

    let first = run(&e, &sink, &msg, 2_100).await.unwrap();
    assert_eq!(first, Handled::Retry("busy".into()));
    assert!(!first.is_finished());
    assert_eq!(status(&e, 2_100).await, [TurnStatus::Ready], "the answer must still be there");

    run(&e, &sink, &msg, 2_200).await.unwrap();
    assert_eq!(run(&e, &sink, &msg, 2_300).await.unwrap(), Handled::Delivered);
    assert_eq!(status(&e, 2_300).await, [TurnStatus::Resumed]);

    let keys: Vec<_> = sink.seen().iter().map(Delivery::idempotency_key).collect();
    assert_eq!(keys.len(), 3);
    assert!(keys.iter().all(|k| k == &keys[0]), "every redelivery is the same delivery: {keys:?}");
}

/// A store that can be told to refuse writes, to stand in for a resumer that dies
/// after the sink accepted and before it closed the ticket.
struct Flaky {
    inner: MemParkStore,
    refuse_writes: Arc<AtomicBool>,
}

impl ParkStore for Flaky {
    async fn get(&self, t: &str) -> Result<Option<(Record, Revision)>> {
        self.inner.get(t).await
    }
    async fn create(&self, t: &str, r: &Record) -> Result<bool> {
        self.inner.create(t, r).await
    }
    async fn cas(&self, t: &str, expected: Revision, r: &Record) -> Result<bool> {
        if self.refuse_writes.load(Ordering::SeqCst) {
            return Err(ParkError::storage("injected: the process died here"));
        }
        self.inner.cas(t, expected, r).await
    }
    async fn claim_correlation(&self, c: &str, t: &str) -> Result<Option<TicketId>> {
        self.inner.claim_correlation(c, t).await
    }
    async fn find_correlation(&self, c: &str) -> Result<Option<TicketId>> {
        self.inner.find_correlation(c).await
    }
    async fn index_session(&self, s: &str, t: &str) -> Result<()> {
        self.inner.index_session(s, t).await
    }
    async fn list_session(&self, s: &str) -> Result<Vec<TicketId>> {
        self.inner.list_session(s).await
    }
    async fn list_tickets(&self) -> Result<Vec<TicketId>> {
        self.inner.list_tickets().await
    }
}

/// The reason delivery reads before it takes: dying between the sink accepting
/// and the ticket closing must redeliver, not lose the answer.
#[tokio::test]
async fn dying_after_the_sink_accepted_redelivers_the_same_delivery() {
    let refuse_writes = Arc::new(AtomicBool::new(false));
    let e = Engine::new(Flaky { inner: MemParkStore::new(), refuse_writes: refuse_writes.clone() });
    let t = park(&e, "c", ReplayPolicy::Unsafe, None).await;
    e.wake("c", answer("42"), 2_000).await.unwrap();
    let sink = Recorder::default();
    let msg = WakeMessage::resolved(S, &t);

    refuse_writes.store(true, Ordering::SeqCst);
    let err = run(&e, &sink, &msg, 2_100).await.unwrap_err();
    assert!(matches!(err, ParkError::Storage(_)), "{err:?}");
    assert_eq!(sink.seen().len(), 1, "the sink was handed it");

    // The process comes back. The message is redelivered; the ticket is intact.
    refuse_writes.store(false, Ordering::SeqCst);
    assert_eq!(status(&e, 2_200).await, [TurnStatus::Ready]);
    assert_eq!(run(&e, &sink, &msg, 2_300).await.unwrap(), Handled::Delivered);
    assert_eq!(status(&e, 2_300).await, [TurnStatus::Resumed]);

    let seen = sink.seen();
    assert_eq!(seen.len(), 2, "at least once");
    assert_eq!(seen[0].idempotency_key(), seen[1].idempotency_key(), "and a sink can tell");
}

#[tokio::test]
async fn a_sink_with_nobody_there_gets_the_ticket_cancelled_not_retried_forever() {
    let e = mem_engine();
    let t = park(&e, "c", ReplayPolicy::Unsafe, None).await;
    e.wake("c", answer("42"), 2_000).await.unwrap();
    let sink = Recorder::replying([SinkReply::Gone("session closed".into())]);

    let got = run(&e, &sink, &WakeMessage::resolved(S, &t), 2_100).await.unwrap();
    assert_eq!(got, Handled::Gone);
    assert!(got.is_finished());
    assert_eq!(status(&e, 2_200).await, [TurnStatus::Cancelled]);
}

#[tokio::test]
async fn a_redispatch_is_delivered_with_what_the_call_was_and_the_ticket_stays_parked() {
    let e = mem_engine();
    let t = park(&e, "read-1", ReplayPolicy::Safe, Some(1_000)).await;
    e.recover(&sweeper(), &policy(), 2_000).await.unwrap();
    let sink = Recorder::default();

    let got = run(&e, &sink, &WakeMessage::redispatch(S, &t, 1), 2_100).await.unwrap();
    assert_eq!(got, Handled::Delivered);
    assert_eq!(
        sink.seen()[0].event,
        Event::Redispatch { attempt: 1, description: "call read-1".into() }
    );
    // Nothing was consumed: the call has not answered yet.
    assert_eq!(status(&e, 2_200).await, [TurnStatus::Parked]);
    assert_eq!(sink.seen()[0].idempotency_key(), format!("{t}:redispatch:1"));
}

#[tokio::test]
async fn a_redispatch_message_the_ticket_has_moved_past_is_stale() {
    let e = mem_engine();
    let t = park(&e, "read-1", ReplayPolicy::Safe, Some(1_000)).await;
    e.recover(&sweeper(), &policy(), 2_000).await.unwrap(); // attempt 1
    e.recover(&sweeper(), &policy(), 3_200).await.unwrap(); // attempt 2
    let sink = Recorder::default();

    let got = run(&e, &sink, &WakeMessage::redispatch(S, &t, 1), 3_300).await.unwrap();
    assert_eq!(got, Handled::Stale, "attempt 2 superseded it");
    assert!(sink.seen().is_empty());

    // And once the call answers, no re-dispatch is wanted at all.
    e.wake("read-1", answer("x"), 3_400).await.unwrap();
    let got = run(&e, &sink, &WakeMessage::redispatch(S, &t, 2), 3_500).await.unwrap();
    assert_eq!(got, Handled::Stale);
    assert!(sink.seen().is_empty());
}

#[tokio::test]
async fn a_message_about_a_ticket_that_is_not_there_is_stale() {
    let e = mem_engine();
    let sink = Recorder::default();
    let got = run(&e, &sink, &WakeMessage::resolved(S, "no-such-ticket"), 1).await.unwrap();
    assert_eq!(got, Handled::Stale);
}

#[tokio::test]
async fn a_message_from_before_adr_0106_still_reads() {
    // `comp-park` before this published `{session, ticket}` and nothing else.
    let m: WakeMessage = serde_json::from_str(r#"{"session":"s1","ticket":"t1"}"#).unwrap();
    assert_eq!(m.kind, WakeKind::Resolved);
    let m: WakeMessage = serde_json::from_str(
        r#"{"session":"s1","ticket":"t1","kind":{"redispatch":{"attempt":2}}}"#,
    )
    .unwrap();
    assert_eq!(m.kind, WakeKind::Redispatch { attempt: 2 });
}

// ---- repair: what a crash between `create` and the index writes leaves ---------

/// A ticket exactly as `park` leaves it when the process dies right after `create`: the record,
/// and neither index entry.
async fn unindexed(store: &MemParkStore, session: &str, correlation: &str) -> TicketId {
    let ticket = ticket_id(session, correlation);
    let record = Record {
        ticket: ticket.clone(),
        session: session.into(),
        call: call(correlation, ReplayPolicy::Unsafe, None),
        by: Agent::named("turn"),
        status: holon_park::store::StoredStatus::Parked,
        parked_at: 1_000,
        woken_at: None,
        resumed_at: None,
        cancelled_by: None,
        cancelled_at: None,
        result: None,
        redispatches: 0,
        interruption: None,
        announced_at: None,
    };
    assert!(store.create(&ticket, &record).await.unwrap());
    ticket
}

#[tokio::test]
async fn repair_puts_back_the_two_index_entries_a_crash_lost() {
    let store = MemParkStore::new();
    let t = unindexed(&store, S, "c1").await;
    let e = Engine::new(store);

    // As the crash left it: reachable by its id, and by nothing else.
    assert!(e.pending(&S.to_string(), 2_000).await.unwrap().is_empty());
    assert!(matches!(e.wake("c1", answer("x"), 2_000).await.unwrap_err(), ParkError::NotFound(_)));

    let done = e.repair().await.unwrap();
    assert_eq!(done.reindexed, std::slice::from_ref(&t));
    assert_eq!(done.reclaimed, std::slice::from_ref(&t));
    assert!(done.conflicts.is_empty());

    assert_eq!(e.pending(&S.to_string(), 2_000).await.unwrap().len(), 1);
    assert_eq!(e.wake("c1", answer("x"), 2_000).await.unwrap().ticket, t);
}

#[tokio::test]
async fn repair_on_a_healthy_store_writes_nothing_and_says_so() {
    let e = mem_engine();
    park(&e, "a", ReplayPolicy::Unsafe, None).await;
    park(&e, "b", ReplayPolicy::Safe, Some(5_000)).await;

    assert!(e.repair().await.unwrap().is_empty());

    // And after a repair, a second finds nothing left.
    let store = MemParkStore::new();
    unindexed(&store, S, "c").await;
    let e = Engine::new(store);
    assert!(!e.repair().await.unwrap().is_empty());
    assert_eq!(e.repair().await.unwrap(), Repaired::default());
}

#[tokio::test]
async fn repair_reports_a_correlation_two_sessions_both_got_instead_of_picking_one() {
    // The second session got in because the first one's claim was never made.
    let store = MemParkStore::new();
    let a = unindexed(&store, "sA", "shared").await;
    let b = unindexed(&store, "sB", "shared").await;
    let e = Engine::new(store);

    let done = e.repair().await.unwrap();
    let mut losers = done.conflicts.clone();
    losers.sort();
    // Whichever claimed first holds it; the other is reported, not silently re-pointed.
    assert_eq!(done.reclaimed.len(), 1, "{done:?}");
    assert_eq!(losers.len(), 1, "{done:?}");
    assert!([&a, &b].contains(&&losers[0]));
    assert!(!done.reclaimed.contains(&losers[0]));
    // Both are visible to their own session either way.
    assert_eq!(e.pending(&"sA".to_string(), 2_000).await.unwrap().len(), 1);
    assert_eq!(e.pending(&"sB".to_string(), 2_000).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_park_retry_after_a_crash_repairs_its_own_ticket_and_refuses_a_stolen_correlation() {
    let store = MemParkStore::new();
    unindexed(&store, S, "c1").await;
    let e = Engine::new(store);

    // The caller comes back and re-runs its park: that alone repairs.
    let again = e
        .park(&S.to_string(), call("c1", ReplayPolicy::Unsafe, None), Agent::named("turn"), 2_000)
        .await
        .unwrap();
    assert_eq!(again.outcome, holon_park::model::ParkOutcome::AlreadyParked);
    assert_eq!(e.pending(&S.to_string(), 2_000).await.unwrap().len(), 1);
    assert_eq!(e.wake("c1", answer("x"), 2_000).await.unwrap().ticket, again.ticket);

    // But if another session took the correlation in the gap, the retry is told so.
    let store = MemParkStore::new();
    unindexed(&store, "sA", "shared").await;
    store.claim_correlation("shared", &ticket_id("sB", "shared")).await.unwrap();
    let e = Engine::new(store);
    let err = e
        .park(
            &"sA".to_string(),
            call("shared", ReplayPolicy::Unsafe, None),
            Agent::named("a"),
            2_000,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ParkError::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn a_sweep_repairs_before_it_recovers_and_reports_both() {
    let store = MemParkStore::new();
    let t = unindexed(&store, S, "c1").await;
    let e = Engine::new(store);
    let said = Said::default();

    let swept = sweep(&e, &said, &sweeper(), &policy(), 2_000).await.unwrap();
    assert_eq!(swept.repaired.reindexed, [t]);
    assert!(swept.recovered.is_empty(), "no deadline: nothing to give up on");
}
