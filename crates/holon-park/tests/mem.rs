//! Every scenario against the in-memory store. Always runs — there is no
//! other backend yet (ADR-0100: NATS is the next step, not this one).

use holon_park::error::ParkError;
use holon_park::model::{Agent, CallResult, OutboundCall, ParkOutcome, TurnStatus};
use holon_park::mem_engine;

fn agent(id: &str) -> Agent {
    Agent::named(id)
}

fn call(correlation: &str) -> OutboundCall {
    OutboundCall { correlation: correlation.into(), description: "test call".into(), deadline: None, poll: None }
}

fn call_with_deadline(correlation: &str, deadline: u64) -> OutboundCall {
    OutboundCall { correlation: correlation.into(), description: "test call".into(), deadline: Some(deadline), poll: None }
}

fn result(ok: bool, body: &str) -> CallResult {
    CallResult { ok, body: body.as_bytes().to_vec(), detail: None }
}

#[tokio::test]
async fn park_then_wake_then_take_ready() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    assert_eq!(p.outcome, ParkOutcome::Parked);

    // Not ready yet.
    let err = e.take_ready(&p.ticket, 1500).await.unwrap_err();
    assert!(matches!(err, ParkError::NotFound(_)), "{err:?}");

    let woken = e.wake("req-1", result(true, "42"), 1200).await.unwrap();
    assert_eq!(woken.ticket, p.ticket);
    assert!(woken.freshly_woken);

    let r = e.take_ready(&p.ticket, 1500).await.unwrap();
    assert!(r.ok);
    assert_eq!(r.body, b"42");

    // Exactly once.
    let err = e.take_ready(&p.ticket, 1600).await.unwrap_err();
    assert!(matches!(err, ParkError::AlreadyClosed(_)), "{err:?}");
}

#[tokio::test]
async fn reparking_the_identical_call_is_idempotent() {
    let e = mem_engine();
    let first = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    assert_eq!(first.outcome, ParkOutcome::Parked);

    // A caller that crashed right after the first `park` and retried.
    let second = e.park(&"s1".into(), call("req-1"), agent("a1"), 1001).await.unwrap();
    assert_eq!(second.ticket, first.ticket);
    assert_eq!(second.outcome, ParkOutcome::AlreadyParked);

    // Only one entry exists — the retry did not create a second record.
    let pending = e.pending(&"s1".into(), 1002).await.unwrap();
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn reparking_after_the_answer_beat_it_reports_already_woken() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    e.wake("req-1", result(true, "42"), 1100).await.unwrap();

    let retry = e.park(&"s1".into(), call("req-1"), agent("a1"), 1200).await.unwrap();
    assert_eq!(retry.ticket, p.ticket);
    assert_eq!(retry.outcome, ParkOutcome::AlreadyWoken);

    // The caller can take it immediately without ever seeing a wake message.
    let r = e.take_ready(&retry.ticket, 1300).await.unwrap();
    assert!(r.ok);
}

#[tokio::test]
async fn waking_an_unknown_correlation_is_not_found() {
    let e = mem_engine();
    let err = e.wake("nothing-parked-this", result(true, "x"), 1000).await.unwrap_err();
    assert!(matches!(err, ParkError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn a_redelivered_wake_is_accepted_not_an_error() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    e.wake("req-1", result(true, "42"), 1100).await.unwrap();
    // The webhook fires again with the same payload.
    let again = e.wake("req-1", result(true, "42"), 1150).await.unwrap();
    assert_eq!(again.ticket, p.ticket);
    assert!(!again.freshly_woken, "a redelivery must not report a fresh wake");

    let r = e.take_ready(&p.ticket, 1200).await.unwrap();
    assert!(r.ok);
}

#[tokio::test]
async fn cancel_then_a_late_wake_is_accepted_and_dropped() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    e.cancel(&p.ticket, agent("a1"), 1100).await.unwrap();

    // Accepted, not an error — but the ticket stays cancelled, not ready.
    let woken = e.wake("req-1", result(true, "late"), 1200).await.unwrap();
    assert_eq!(woken.ticket, p.ticket);
    assert!(!woken.freshly_woken, "a wake against an already-cancelled ticket is not a fresh wake");

    let err = e.take_ready(&p.ticket, 1300).await.unwrap_err();
    assert!(matches!(err, ParkError::AlreadyClosed(_)), "{err:?}");

    let pending = e.pending(&"s1".into(), 1300).await.unwrap();
    assert!(pending.is_empty(), "a cancelled ticket must not show up as pending: {pending:?}");
}

#[tokio::test]
async fn cancel_is_not_idempotent_past_a_terminal_state() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call("req-1"), agent("a1"), 1000).await.unwrap();
    e.wake("req-1", result(true, "42"), 1100).await.unwrap();
    e.take_ready(&p.ticket, 1200).await.unwrap();

    let err = e.cancel(&p.ticket, agent("a1"), 1300).await.unwrap_err();
    assert!(matches!(err, ParkError::AlreadyClosed(_)), "{err:?}");
}

#[tokio::test]
async fn a_ticket_past_its_deadline_reads_expired_and_stops_showing_as_pending() {
    let e = mem_engine();
    let p = e.park(&"s1".into(), call_with_deadline("req-1", 2000), agent("a1"), 1000).await.unwrap();

    // Before the deadline: still pending.
    let pending = e.pending(&"s1".into(), 1500).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, TurnStatus::Parked);

    // Past the deadline: gone from pending, but the oplog still shows it —
    // now as `expired`, computed purely from `now_ms`, no sleep needed.
    let pending = e.pending(&"s1".into(), 2500).await.unwrap();
    assert!(pending.is_empty(), "{pending:?}");
    let log = e.oplog(&"s1".into(), None, 10, 2500).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].status, TurnStatus::Expired);

    // A late wake still lands — expired is a read, not a terminal write.
    let woken = e.wake("req-1", result(true, "late but real"), 2600).await.unwrap();
    assert_eq!(woken.ticket, p.ticket);
    assert!(woken.freshly_woken, "a late but real wake against a still-`parked` (only read as expired) ticket must land");
    let r = e.take_ready(&p.ticket, 2700).await.unwrap();
    assert!(r.ok);
}

#[tokio::test]
async fn the_same_correlation_for_two_different_sessions_is_refused() {
    let e = mem_engine();
    e.park(&"s1".into(), call("shared-req"), agent("a1"), 1000).await.unwrap();

    let err = e.park(&"s2".into(), call("shared-req"), agent("a2"), 1001).await.unwrap_err();
    assert!(matches!(err, ParkError::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn oplog_orders_by_park_time_and_respects_after_and_limit() {
    let e = mem_engine();
    e.park(&"s1".into(), call("req-a"), agent("a1"), 3000).await.unwrap();
    e.park(&"s1".into(), call("req-b"), agent("a1"), 1000).await.unwrap();
    e.park(&"s1".into(), call("req-c"), agent("a1"), 2000).await.unwrap();

    let all = e.oplog(&"s1".into(), None, 10, 4000).await.unwrap();
    let times: Vec<u64> = all.iter().map(|e| e.parked_at).collect();
    assert_eq!(times, vec![1000, 2000, 3000], "must come back oldest first regardless of park order");

    let after = e.oplog(&"s1".into(), Some(1000), 10, 4000).await.unwrap();
    assert_eq!(after.iter().map(|e| e.parked_at).collect::<Vec<_>>(), vec![2000, 3000]);

    let limited = e.oplog(&"s1".into(), None, 2, 4000).await.unwrap();
    assert_eq!(limited.len(), 2);
}

/// Many agents racing to park the identical call at once: exactly one record,
/// every caller told the same ticket, and either `Parked` or `AlreadyParked` —
/// never a duplicate and never an error. The same shape as
/// `holon-vcs`'s own N-way race tests (`scenario_b_race_repeated`), because
/// the ticket id doing the arbitration is the same idea as a vcs pointer key.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn n_agents_racing_to_park_the_same_call_land_on_one_ticket() {
    use std::sync::Arc;

    let e = Arc::new(mem_engine());
    let mut handles = Vec::new();
    for i in 0..16 {
        let e = e.clone();
        handles.push(tokio::spawn(async move {
            e.park(&"s1".into(), call("shared"), agent(&format!("a{i}")), 1000 + i as u64).await.unwrap()
        }));
    }
    let mut tickets = std::collections::HashSet::new();
    let mut parked_count = 0;
    for h in handles {
        let r = h.await.unwrap();
        tickets.insert(r.ticket);
        if r.outcome == ParkOutcome::Parked {
            parked_count += 1;
        }
    }
    assert_eq!(tickets.len(), 1, "every racer must be told the same ticket");
    assert_eq!(parked_count, 1, "exactly one racer creates the record");

    let pending = e.pending(&"s1".into(), 2000).await.unwrap();
    assert_eq!(pending.len(), 1, "no duplicate record: {pending:?}");
}
