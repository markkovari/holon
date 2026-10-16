//! Groups (ADR-0109), against the in-memory store: registration, the join, the deadline, the
//! cascade, what is announced, and the races. Time is injected everywhere, so nothing sleeps.

use std::sync::{Arc, Mutex};

use holon_park::error::ParkError;
use holon_park::mem::MemParkStore;
use holon_park::model::{
    Agent, CallResult, ChildResult, GroupSpec, JoinOutcome, JoinPolicy, OutboundCall, ParkOutcome,
    ReplayPolicy, Resolution, TicketId, TurnStatus, WakeMessage, MAX_GROUP_CHILDREN,
};
use holon_park::recover::{RecoveryAction, RecoveryPolicy};
use holon_park::resume::{handle, sweep, Announcer, Delivery, Event, Handled, Sink, SinkReply};
use holon_park::store::{ParkStore, Record, StoredStatus};
use holon_park::ticket::ticket_id;
use holon_park::{mem_engine, Engine, MemEngine};

const S: &str = "s1";

fn session() -> String {
    S.to_string()
}

fn sweeper() -> Agent {
    Agent::named("comp-resume")
}

fn policy() -> RecoveryPolicy {
    RecoveryPolicy {
        deadline_grace_ms: 0,
        max_redispatches: 3,
        retry_after_ms: 1_000,
        reannounce_after_ms: 500,
    }
}

fn t(correlation: &str) -> TicketId {
    ticket_id(S, correlation)
}

fn answer(ok: bool) -> CallResult {
    CallResult { ok, body: vec![ok as u8], detail: None }
}

fn call(correlation: &str) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: format!("call {correlation}"),
        deadline: None,
        poll: None,
        replay: ReplayPolicy::Unsafe,
        parent: None,
        group: None,
    }
}

fn group_call(correlation: &str, policy: JoinPolicy, children: u32) -> OutboundCall {
    OutboundCall { group: Some(GroupSpec { policy, children }), ..call(correlation) }
}

fn child_call(correlation: &str, parent: &str) -> OutboundCall {
    OutboundCall { parent: Some(parent.into()), ..call(correlation) }
}

async fn park<St: ParkStore>(
    e: &Engine<St>,
    call: OutboundCall,
    now: u64,
) -> Result<holon_park::model::ParkResult, ParkError> {
    e.park(&session(), call, Agent::named("turn"), now).await
}

/// A group parked, then its children in order, each a millisecond after the last.
async fn fan_out(
    e: &MemEngine,
    policy: JoinPolicy,
    deadline: Option<u64>,
    children: &[&str],
) -> TicketId {
    let g = OutboundCall { deadline, ..group_call("g", policy, children.len() as u32) };
    park(e, g, 1_000).await.unwrap();
    for (i, c) in children.iter().enumerate() {
        park(e, child_call(c, "g"), 1_001 + i as u64).await.unwrap();
    }
    t("g")
}

async fn status_of(e: &MemEngine, correlation: &str) -> TurnStatus {
    let log = e.oplog(&session(), None, 100, 1_000_000).await.unwrap();
    log.into_iter().find(|x| x.correlation == correlation).expect("in the oplog").status
}

fn joined(r: Resolution) -> holon_park::model::JoinResult {
    match r {
        Resolution::Joined(j) => j,
        other => panic!("expected a joined resolution, got {other:?}"),
    }
}

// ---- registration -------------------------------------------------------------------

#[tokio::test]
async fn a_group_takes_the_children_it_was_declared_with_and_no_more() {
    let e = mem_engine();
    park(&e, group_call("g", JoinPolicy::AllSettled, 2), 1_000).await.unwrap();

    let a = park(&e, child_call("c1", "g"), 1_001).await.unwrap();
    assert_eq!(a.outcome, ParkOutcome::Parked);
    park(&e, child_call("c2", "g"), 1_002).await.unwrap();

    let err = park(&e, child_call("c3", "g"), 1_003).await.unwrap_err();
    assert!(matches!(&err, ParkError::Invalid(m) if m.contains("full")), "{err:?}");

    // The same child again is the same ticket, not a place taken twice.
    let again = park(&e, child_call("c1", "g"), 1_004).await.unwrap();
    assert_eq!((again.ticket, again.outcome), (a.ticket, ParkOutcome::AlreadyParked));

    // And what a client reads says who belongs to whom.
    let log = e.oplog(&session(), None, 10, 2_000).await.unwrap();
    let by = |c: &str| log.iter().find(|x| x.correlation == c).unwrap().clone();
    assert_eq!(by("g").group, Some(GroupSpec { policy: JoinPolicy::AllSettled, children: 2 }));
    assert_eq!(by("g").parent, None);
    assert_eq!(by("c1").parent, Some(t("g")));
    assert_eq!(by("c1").group, None);
}

#[tokio::test]
async fn what_a_call_may_not_say_about_groups_is_refused() {
    let e = mem_engine();
    let bad = |what: &str, r: Result<_, ParkError>| {
        assert!(matches!(&r, Err(ParkError::Invalid(_))), "{what}: {r:?}");
    };

    bad("a child with no group", park(&e, child_call("c", "nope"), 1).await);
    park(&e, call("plain"), 1).await.unwrap();
    bad("a child of a call that is not a group", park(&e, child_call("c", "plain"), 2).await);

    park(&e, group_call("g", JoinPolicy::AllSettled, 2), 3).await.unwrap();
    bad(
        "a group inside a group",
        park(
            &e,
            OutboundCall {
                parent: Some("g".into()),
                ..group_call("g2", JoinPolicy::AllSettled, 1)
            },
            4,
        )
        .await,
    );
    bad("a group of none", park(&e, group_call("g0", JoinPolicy::FailFast, 0), 5).await);
    bad(
        "a group of too many",
        park(&e, group_call("gm", JoinPolicy::FailFast, MAX_GROUP_CHILDREN + 1), 6).await,
    );
    bad("a call that is its own parent", park(&e, child_call("g", "g"), 7).await);

    // A child that exists cannot be re-parked under another group, or none.
    park(&e, group_call("h", JoinPolicy::AllSettled, 1), 8).await.unwrap();
    park(&e, child_call("c1", "g"), 9).await.unwrap();
    bad("a child moved to another group", park(&e, child_call("c1", "h"), 10).await);
    bad("a child made a plain call", park(&e, call("c1"), 11).await);
}

#[tokio::test]
async fn a_group_belongs_to_its_session() {
    let e = mem_engine();
    park(&e, group_call("g", JoinPolicy::AllSettled, 1), 1).await.unwrap();
    // The same name in another session is another (absent) group.
    let err =
        e.park(&"s2".to_string(), child_call("c", "g"), Agent::named("turn"), 2).await.unwrap_err();
    assert!(matches!(&err, ParkError::Invalid(m) if m.contains("no group")), "{err:?}");
}

#[tokio::test]
async fn a_child_cannot_join_a_group_that_has_already_resolved() {
    let e = mem_engine();
    fan_out(&e, JoinPolicy::FailFast, None, &["c1", "c2"]).await;
    e.interrupt(&t("c1"), "boom", Agent::named("turn"), 2_000).await.unwrap(); // fail-fast: resolved

    park(&e, child_call("late", "g"), 2_001).await.unwrap_err();
    assert!(e
        .oplog(&session(), None, 10, 3_000)
        .await
        .unwrap()
        .iter()
        .all(|x| x.correlation != "late" || x.status == TurnStatus::Cancelled));
}

// ---- the join ----------------------------------------------------------------------------

#[tokio::test]
async fn an_all_settled_group_resolves_when_its_last_child_settles_and_reports_each() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["c1", "c2", "c3"]).await;

    let w = e.wake("c1", answer(true), 2_000).await.unwrap();
    assert!(w.freshly_woken && w.announce.is_empty(), "a child is never announced: {w:?}");
    let w = e.wake("c2", answer(false), 2_001).await.unwrap();
    assert!(w.announce.is_empty(), "the group is still waiting on c3: {w:?}");
    assert_eq!(status_of(&e, "g").await, TurnStatus::Parked);

    let i = e.interrupt(&t("c3"), "gave up", Agent::named("turn"), 2_002).await.unwrap();
    assert_eq!(i.announce, [WakeMessage::resolved(S, &g)], "the last child resolves the group");
    assert_eq!(status_of(&e, "g").await, TurnStatus::Ready);

    // Every child, in the order it was parked, and how each ended.
    let j = joined(e.resolution(&g).await.unwrap());
    assert_eq!((j.outcome, j.missing), (JoinOutcome::SomeFailed, 0));
    let corr: Vec<_> = j.children.iter().map(|c| c.correlation.as_str()).collect();
    assert_eq!(corr, ["c1", "c2", "c3"]);
    assert!(matches!(&j.children[0].ended, ChildResult::Answered(a) if a.ok));
    assert!(matches!(&j.children[1].ended, ChildResult::Answered(a) if !a.ok));
    assert!(matches!(&j.children[2].ended, ChildResult::Interrupted(i) if i.reason == "gave up"));

    // The children were consumed by the group: none lingers as an answer nobody will take.
    for c in ["c1", "c2", "c3"] {
        assert_eq!(status_of(&e, c).await, TurnStatus::Resumed, "{c}");
    }
    assert_eq!(
        e.pending(&session(), 3_000)
            .await
            .unwrap()
            .iter()
            .map(|p| p.correlation.clone())
            .collect::<Vec<_>>(),
        ["g"]
    );

    // Taking the group is what a turn does, once.
    assert_eq!(joined(e.take_ready(&g, 3_001).await.unwrap()), j);
    assert!(matches!(e.take_ready(&g, 3_002).await.unwrap_err(), ParkError::AlreadyClosed(_)));
}

#[tokio::test]
async fn every_child_answering_well_is_all_answered() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["a", "b"]).await;
    e.wake("a", answer(true), 2_000).await.unwrap();
    let w = e.wake("b", answer(true), 2_001).await.unwrap();
    assert_eq!(w.announce, [WakeMessage::resolved(S, &g)]);
    assert_eq!(joined(e.take_ready(&g, 3_000).await.unwrap()).outcome, JoinOutcome::AllAnswered);
}

#[tokio::test]
async fn a_group_waits_for_children_it_has_not_been_given_yet() {
    let e = mem_engine();
    park(&e, group_call("g", JoinPolicy::AllSettled, 3), 1_000).await.unwrap();
    for (i, c) in ["a", "b"].iter().enumerate() {
        park(&e, child_call(c, "g"), 1_001 + i as u64).await.unwrap();
        e.wake(c, answer(true), 1_100 + i as u64).await.unwrap();
    }
    // Two of three settled: not complete, however settled they are.
    assert_eq!(status_of(&e, "g").await, TurnStatus::Parked);

    park(&e, child_call("c", "g"), 1_200).await.unwrap();
    let w = e.wake("c", answer(true), 1_201).await.unwrap();
    assert_eq!(w.announce.len(), 1);
    assert_eq!(status_of(&e, "g").await, TurnStatus::Ready);
}

#[tokio::test]
async fn a_fail_fast_group_cancels_what_is_still_running_before_it_resolves() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::FailFast, None, &["c1", "c2", "c3"]).await;

    let w = e.wake("c1", answer(true), 2_000).await.unwrap();
    assert!(w.announce.is_empty(), "an answer that is not a failure does not resolve it");
    let w = e.wake("c2", answer(false), 2_001).await.unwrap();
    assert_eq!(w.announce, [WakeMessage::resolved(S, &g)], "the first failure does");

    let j = joined(e.take_ready(&g, 3_000).await.unwrap());
    assert_eq!(j.outcome, JoinOutcome::FailedFast);
    assert!(matches!(&j.children[0].ended, ChildResult::Answered(a) if a.ok), "c1 kept its answer");
    assert!(matches!(&j.children[1].ended, ChildResult::Answered(a) if !a.ok));
    assert_eq!(j.children[2].ended, ChildResult::Cancelled, "c3 was still running");
    assert_eq!(status_of(&e, "c3").await, TurnStatus::Cancelled);

    // The answer c3 sends anyway is accepted and dropped, like any late answer.
    let late = e.wake("c3", answer(true), 4_000).await.unwrap();
    assert!(!late.freshly_woken && late.announce.is_empty());
    assert_eq!(status_of(&e, "c3").await, TurnStatus::Cancelled);
}

#[tokio::test]
async fn a_failure_of_any_kind_trips_fail_fast() {
    for how in ["interrupt", "cancel"] {
        let e = mem_engine();
        let g = fan_out(&e, JoinPolicy::FailFast, None, &["c1", "c2"]).await;
        let announce = match how {
            "interrupt" => {
                e.interrupt(&t("c1"), "boom", Agent::named("turn"), 2_000).await.unwrap().announce
            }
            _ => e.cancel_announcing(&t("c1"), Agent::named("turn"), 2_000).await.unwrap(),
        };
        assert_eq!(announce, [WakeMessage::resolved(S, &g)], "{how}");
        assert_eq!(
            joined(e.resolution(&g).await.unwrap()).outcome,
            JoinOutcome::FailedFast,
            "{how}"
        );
    }
}

// ---- what a child and a group refuse ------------------------------------------------------

#[tokio::test]
async fn a_child_is_consumed_by_its_group_and_a_group_is_resolved_by_its_children() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["c1", "c2"]).await;
    e.wake("c1", answer(true), 2_000).await.unwrap();

    for (what, r) in [
        ("take a child", e.take_ready(&t("c1"), 2_001).await.map(|_| ())),
        ("read a child's resolution", e.resolution(&t("c1")).await.map(|_| ())),
        ("wake a group", e.wake("g", answer(true), 2_002).await.map(|_| ())),
        ("interrupt a group", e.interrupt(&g, "x", Agent::named("turn"), 2_003).await.map(|_| ())),
    ] {
        assert!(matches!(&r, Err(ParkError::Invalid(_))), "{what}: {r:?}");
    }
    // None of that moved anything.
    assert_eq!(status_of(&e, "c1").await, TurnStatus::Ready);
    assert_eq!(status_of(&e, "g").await, TurnStatus::Parked);
}

// ---- the deadline ------------------------------------------------------------------------------

#[tokio::test]
async fn a_group_past_its_own_deadline_cancels_what_is_left_and_resolves_timed_out() {
    let e = mem_engine();
    park(
        &e,
        OutboundCall { deadline: Some(5_000), ..group_call("g", JoinPolicy::AllSettled, 3) },
        1_000,
    )
    .await
    .unwrap();
    park(&e, child_call("c1", "g"), 1_001).await.unwrap();
    park(&e, child_call("c2", "g"), 1_002).await.unwrap();
    e.wake("c1", answer(true), 1_100).await.unwrap();
    // c3 is never parked.

    assert!(
        e.recover(&sweeper(), &policy(), 4_999).await.unwrap().is_empty(),
        "before the deadline"
    );
    let done = e.recover(&sweeper(), &policy(), 5_000).await.unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!((done[0].action.clone(), done[0].ticket.clone()), (RecoveryAction::Joined, t("g")));
    assert_eq!(done[0].announcement(), Some(WakeMessage::resolved(S, t("g"))));

    let j = joined(e.take_ready(&t("g"), 6_000).await.unwrap());
    assert_eq!((j.outcome, j.missing), (JoinOutcome::TimedOut, 1));
    assert!(matches!(&j.children[0].ended, ChildResult::Answered(a) if a.ok));
    assert_eq!(j.children[1].ended, ChildResult::Cancelled);
    assert_eq!(status_of(&e, "c2").await, TurnStatus::Cancelled);

    // A pass over a settled bucket does nothing more.
    assert!(e.recover(&sweeper(), &policy(), 9_000).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_joins_own_answer_beats_a_deadline_that_also_passed() {
    // Both children settled and the group's deadline passed before anything ran the join: the
    // outcome is what the children did, not "timed out".
    let e = mem_engine();
    park(
        &e,
        OutboundCall { deadline: Some(5_000), ..group_call("g", JoinPolicy::AllSettled, 1) },
        1_000,
    )
    .await
    .unwrap();
    park(&e, OutboundCall { deadline: Some(2_000), ..child_call("c1", "g") }, 1_001).await.unwrap();

    // One pass: the child (unsafe, past its deadline) is interrupted, then the group joins it.
    let done = e.recover(&sweeper(), &policy(), 6_000).await.unwrap();
    let by: Vec<_> = done
        .iter()
        .map(|d| (d.correlation.as_str(), d.action.clone(), d.announcement().is_some()))
        .collect();
    assert_eq!(
        by,
        [("c1", RecoveryAction::Interrupted, false), ("g", RecoveryAction::Joined, true)],
        "the child is not announced; its group is"
    );
    assert_eq!(
        joined(e.take_ready(&t("g"), 7_000).await.unwrap()).outcome,
        JoinOutcome::SomeFailed
    );
}

// ---- cancelling ----------------------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_group_cancels_its_children_and_then_itself() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["c1", "c2", "c3"]).await;
    e.wake("c1", answer(true), 2_000).await.unwrap();
    e.interrupt(&t("c2"), "x", Agent::named("turn"), 2_001).await.unwrap();

    let said = e.cancel_announcing(&g, Agent::named("turn"), 3_000).await.unwrap();
    assert!(said.is_empty(), "a cancelled group says nothing");
    for c in ["g", "c1", "c2", "c3"] {
        assert_eq!(status_of(&e, c).await, TurnStatus::Cancelled, "{c}");
    }
    // Again: closed, and nothing left running.
    assert!(matches!(
        e.cancel(&g, Agent::named("turn"), 3_001).await.unwrap_err(),
        ParkError::AlreadyClosed(_)
    ));
    assert!(e.pending(&session(), 4_000).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_group_that_already_resolved_can_still_be_cancelled_and_its_children_stay_closed() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["c1"]).await;
    e.wake("c1", answer(true), 2_000).await.unwrap();
    assert_eq!(status_of(&e, "g").await, TurnStatus::Ready);

    e.cancel(&g, Agent::named("turn"), 3_000).await.unwrap();
    assert_eq!(status_of(&e, "g").await, TurnStatus::Cancelled);
    assert_eq!(status_of(&e, "c1").await, TurnStatus::Resumed, "already consumed: left as it was");
}

// ---- races -------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn children_answering_at_once_resolve_the_group_exactly_once() {
    for round in 0..20 {
        let e = Arc::new(mem_engine());
        let names: Vec<String> = (0..8).map(|i| format!("c{i}")).collect();
        park(&e, group_call("g", JoinPolicy::AllSettled, 8), 1_000).await.unwrap();
        for (i, c) in names.iter().enumerate() {
            park(&e, child_call(c, "g"), 1_001 + i as u64).await.unwrap();
        }

        let mut handles = Vec::new();
        for c in names.clone() {
            let e = e.clone();
            handles.push(tokio::spawn(async move {
                e.wake(&c, answer(true), 2_000).await.unwrap().announce
            }));
        }
        let mut announced = Vec::new();
        for h in handles {
            announced.extend(h.await.unwrap());
        }
        assert_eq!(announced, [WakeMessage::resolved(S, t("g"))], "round {round}: announced once");

        let j = joined(e.take_ready(&t("g"), 3_000).await.unwrap());
        assert_eq!((j.outcome, j.children.len()), (JoinOutcome::AllAnswered, 8), "round {round}");
        let log = e.oplog(&session(), None, 100, 4_000).await.unwrap();
        assert!(
            log.iter().filter(|x| x.parent.is_some()).all(|x| x.status == TurnStatus::Resumed),
            "round {round}: every child consumed"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn children_racing_for_the_last_places_cannot_exceed_the_group() {
    let e = Arc::new(mem_engine());
    park(&e, group_call("g", JoinPolicy::AllSettled, 3), 1_000).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..12 {
        let e = e.clone();
        handles.push(tokio::spawn(async move {
            park(&e, child_call(&format!("c{i}"), "g"), 1_001 + i).await
        }));
    }
    let (mut taken, mut refused) = (0, 0);
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => taken += 1,
            Err(ParkError::Invalid(m)) if m.contains("full") => refused += 1,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!((taken, refused), (3, 9));
}

// ---- the resumer ------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Rec(Mutex<Vec<Delivery>>);

impl Sink for Rec {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        self.0.lock().unwrap().push(d.clone());
        SinkReply::Delivered
    }
}

#[tokio::test]
async fn a_group_is_delivered_as_one_joined_event_and_its_children_are_not_delivered_at_all() {
    let e = mem_engine();
    let g = fan_out(&e, JoinPolicy::AllSettled, None, &["c1", "c2"]).await;
    e.wake("c1", answer(true), 2_000).await.unwrap();
    let w = e.wake("c2", answer(true), 2_001).await.unwrap();
    let sink = Rec::default();

    // What the daemon would have put on PARK_WAKE for those two wakes: the group, once.
    assert_eq!(w.announce, [WakeMessage::resolved(S, &g)]);
    let got = handle(&e, &sink, &w.announce[0], &sweeper(), 3_000).await.unwrap();
    assert_eq!(got, Handled::Delivered);
    let seen = sink.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].idempotency_key(), format!("{g}:joined"));
    let Event::Joined { joined } = &seen[0].event else { panic!("{:?}", seen[0]) };
    assert_eq!((joined.outcome, joined.children.len()), (JoinOutcome::AllAnswered, 2));
    assert_eq!(status_of(&e, "g").await, TurnStatus::Resumed);

    // A leftover message naming a child is stale: it is not the child's to deliver.
    let stale =
        handle(&e, &sink, &WakeMessage::resolved(S, t("c1")), &sweeper(), 3_001).await.unwrap();
    assert_eq!(stale, Handled::Stale);
    assert_eq!(sink.0.lock().unwrap().len(), 1);
}

#[derive(Default)]
struct Said(Mutex<Vec<WakeMessage>>);

impl Announcer for Said {
    async fn announce(&self, msg: &WakeMessage) -> holon_park::error::Result<()> {
        self.0.lock().unwrap().push(msg.clone());
        Ok(())
    }
}

#[tokio::test]
async fn a_sweep_announces_a_group_it_resolved_and_never_the_child_that_tipped_it() {
    let e = mem_engine();
    park(&e, group_call("g", JoinPolicy::FailFast, 2), 1_000).await.unwrap();
    park(&e, OutboundCall { deadline: Some(2_000), ..child_call("c1", "g") }, 1_001).await.unwrap();
    park(&e, child_call("c2", "g"), 1_002).await.unwrap();
    let said = Said::default();

    let swept = sweep(&e, &said, &sweeper(), &policy(), 3_000).await.unwrap();
    assert_eq!(swept.recovered.len(), 2, "{:?}", swept.recovered);
    assert_eq!(*said.0.lock().unwrap(), [WakeMessage::resolved(S, t("g"))]);
    assert_eq!(status_of(&e, "c2").await, TurnStatus::Cancelled, "fail-fast cancelled the other");
}

// ---- repair -----------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn repair_lists_a_child_the_crash_left_out_of_its_group() {
    // A child as `park` leaves it when the process dies right after `create`: the record, and
    // none of the three index entries. Written through the store, as the crash would.
    let store = MemParkStore::new();
    let e0 = Engine::new(MemParkStore::new());
    drop(e0);
    let g = t("g");
    let group = Record {
        ticket: g.clone(),
        session: session(),
        call: group_call("g", JoinPolicy::AllSettled, 1),
        by: Agent::named("turn"),
        status: StoredStatus::Parked,
        parked_at: 1_000,
        woken_at: None,
        resumed_at: None,
        cancelled_by: None,
        cancelled_at: None,
        result: None,
        redispatches: 0,
        interruption: None,
        announced_at: None,
        joined: None,
    };
    let child =
        Record { ticket: t("c"), call: child_call("c", "g"), parked_at: 1_001, ..group.clone() };
    store.create(&g, &group).await.unwrap();
    store.create(&t("c"), &child).await.unwrap();
    let e = Engine::new(store);

    let done = e.repair().await.unwrap();
    assert_eq!(done.reparented, [t("c")], "{done:?}");
    assert!(done.reindexed.contains(&t("c")) && done.reclaimed.contains(&t("c")));

    // And now the group can see it: its answer resolves the group.
    let w = e.wake("c", answer(true), 2_000).await.unwrap();
    assert_eq!(w.announce, [WakeMessage::resolved(S, &g)]);
    assert!(e.repair().await.unwrap().is_empty(), "a second repair finds nothing");
}
