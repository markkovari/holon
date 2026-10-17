//! Commits (ADR-0110), against the in-memory store: admission, the intent, roll-forward, roll-back,
//! what is applied in which order, and what is announced. Time is injected, so nothing sleeps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use holon_park::batch::BatchOutcome;
use holon_park::error::{ParkError, Result};
use holon_park::mem::MemParkStore;
use holon_park::model::{
    Agent, CallResult, Effect, GroupSpec, InterruptEffect, JoinPolicy, OutboundCall, ParkOutcome,
    ReplayPolicy, TicketId, TurnStatus, WakeEffect, WakeMessage, MAX_BATCH_EFFECTS,
};
use holon_park::store::{Batch, BatchState, IndexChild, ParkStore, Record, Revision};
use holon_park::ticket::{batch_id, ticket_id};
use holon_park::Engine;

const S: &str = "s1";

fn session() -> String {
    S.to_string()
}

fn turn() -> Agent {
    Agent::named("turn")
}

fn t(correlation: &str) -> TicketId {
    ticket_id(S, correlation)
}

fn answer() -> CallResult {
    CallResult { ok: true, body: vec![1], detail: None }
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

fn park(correlation: &str) -> Effect {
    Effect::Park(call(correlation))
}

fn group(correlation: &str, children: u32) -> Effect {
    Effect::Park(OutboundCall {
        group: Some(GroupSpec { policy: JoinPolicy::AllSettled, children }),
        ..call(correlation)
    })
}

fn child(correlation: &str, parent: &str) -> Effect {
    Effect::Park(OutboundCall { parent: Some(parent.into()), ..call(correlation) })
}

fn wake(correlation: &str) -> Effect {
    Effect::Wake(WakeEffect { correlation: correlation.into(), answer: answer() })
}

fn interrupt(correlation: &str) -> Effect {
    Effect::Interrupt(InterruptEffect { correlation: correlation.into(), reason: "gave up".into() })
}

fn cancel(correlation: &str) -> Effect {
    Effect::Cancel(correlation.into())
}

async fn status_of<St: ParkStore>(e: &Engine<St>, correlation: &str) -> TurnStatus {
    let log = e.oplog(&session(), None, 100, 1_000_000).await.unwrap();
    log.into_iter().find(|x| x.correlation == correlation).expect("in the oplog").status
}

async fn parked_correlations<St: ParkStore>(e: &Engine<St>) -> Vec<String> {
    let mut v: Vec<_> = e
        .oplog(&session(), None, 100, 1_000_000)
        .await
        .unwrap()
        .into_iter()
        .map(|x| x.correlation)
        .collect();
    v.sort();
    v
}

fn engine() -> Engine<MemParkStore> {
    Engine::new(MemParkStore::new())
}

// ---- the unit -------------------------------------------------------------------------------

#[tokio::test]
async fn a_commit_registers_a_fan_out_as_one_unit() {
    let e = engine();
    let c = e
        .commit(&session(), vec![group("g", 2), child("c1", "g"), child("c2", "g")], turn(), 1_000)
        .await
        .unwrap();

    assert_eq!(c.result.parked.len(), 3);
    assert!(c.result.parked.iter().all(|p| p.outcome == ParkOutcome::Parked));
    assert_eq!(parked_correlations(&e).await, ["c1", "c2", "g"]);
    assert!(c.announce.is_empty());

    // The same list again is the same commit: nothing is made twice, and it says what it did.
    let again = e
        .commit(&session(), vec![group("g", 2), child("c1", "g"), child("c2", "g")], turn(), 2_000)
        .await
        .unwrap();
    assert_eq!(again.result.batch, c.result.batch);
    assert!(again.result.parked.iter().all(|p| p.outcome == ParkOutcome::AlreadyParked));
    assert_eq!(parked_correlations(&e).await.len(), 3);
}

#[tokio::test]
async fn a_commit_is_its_effects_in_order() {
    let e = engine();
    let a = e.commit(&session(), vec![park("a"), park("b")], turn(), 1).await.unwrap();
    let b = e.commit(&session(), vec![park("b"), park("a")], turn(), 2).await.unwrap();
    assert_ne!(a.result.batch, b.result.batch, "a different order is a different commit");
    assert_eq!(a.result.batch, batch_id(S, &[park("a"), park("b")]));
}

// ---- all or nothing admission ------------------------------------------------------------------

#[tokio::test]
async fn a_list_with_one_bad_effect_registers_none_of_them() {
    let e = engine();
    // A second session owns this correlation.
    e.park(&"other".to_string(), call("taken"), turn(), 1).await.unwrap();

    let many: Vec<Effect> = (0..=MAX_BATCH_EFFECTS).map(|i| park(&format!("n{i}"))).collect();
    let cases: Vec<(&str, Vec<Effect>)> = vec![
        ("nothing", vec![]),
        ("too many", many),
        ("a call parked twice", vec![park("a"), park("a")]),
        ("a child of nothing", vec![park("a"), child("c", "nope")]),
        ("a child before its group", vec![child("c", "g"), group("g", 1)]),
        (
            "a group inside a group",
            vec![
                group("g", 1),
                Effect::Park(OutboundCall {
                    parent: Some("g".into()),
                    group: Some(GroupSpec { policy: JoinPolicy::FailFast, children: 1 }),
                    ..call("h")
                }),
            ],
        ),
        ("more children than declared", vec![group("g", 1), child("c1", "g"), child("c2", "g")]),
        ("a correlation another session owns", vec![park("a"), park("taken")]),
        ("a wake of nothing", vec![park("a"), wake("nope")]),
        ("a wake of a group in the commit", vec![group("g", 1), wake("g")]),
        ("an interrupt of a group in the commit", vec![group("g", 1), interrupt("g")]),
    ];
    for (what, effects) in cases {
        let r = e.commit(&session(), effects, turn(), 10).await;
        assert!(matches!(&r, Err(ParkError::Invalid(_))), "{what}: {r:?}");
    }
    // None of those left anything behind: not the good effect in front of the bad one.
    assert!(parked_correlations(&e).await.is_empty());
    assert!(e.complete_batches(0, 1_000_000).await.unwrap().is_empty(), "no intent was written");
}

#[tokio::test]
async fn a_commit_cannot_overfill_a_group_that_is_already_there() {
    let e = engine();
    e.park(
        &session(),
        OutboundCall {
            group: Some(GroupSpec { policy: JoinPolicy::AllSettled, children: 2 }),
            ..call("g")
        },
        turn(),
        1,
    )
    .await
    .unwrap();
    e.park(&session(), OutboundCall { parent: Some("g".into()), ..call("c1") }, turn(), 2)
        .await
        .unwrap();

    // One place left: one more child fits, two do not — and the one that fits is not parked for it.
    let r = e.commit(&session(), vec![child("c2", "g"), child("c3", "g")], turn(), 10).await;
    assert!(matches!(&r, Err(ParkError::Invalid(m)) if m.contains("declared with 2")), "{r:?}");
    assert_eq!(parked_correlations(&e).await, ["c1", "g"]);
    e.commit(&session(), vec![child("c2", "g")], turn(), 11).await.unwrap();
    // A child already there is not a new place.
    e.commit(&session(), vec![child("c1", "g"), child("c2", "g")], turn(), 12).await.unwrap();
}

// ---- what a commit does besides parking ---------------------------------------------------------------------

#[tokio::test]
async fn a_commit_can_replace_one_call_with_another() {
    let e = engine();
    e.park(&session(), call("old"), turn(), 1).await.unwrap();

    // Listed cancel-first, applied park-first: the new call is there before the old one goes.
    e.commit(&session(), vec![cancel("old"), park("new")], turn(), 10).await.unwrap();
    assert_eq!(status_of(&e, "old").await, TurnStatus::Cancelled);
    assert_eq!(status_of(&e, "new").await, TurnStatus::Parked);
}

#[tokio::test]
async fn what_a_commit_does_to_a_childs_group_is_announced_by_the_commit() {
    let e = engine();
    let c = e
        .commit(&session(), vec![group("g", 2), child("c1", "g"), child("c2", "g")], turn(), 1)
        .await
        .unwrap();
    assert!(c.announce.is_empty());

    // One commit answers one child and gives up on the other: the group resolves once, and the
    // commit says so — a child is never announced.
    let c = e.commit(&session(), vec![wake("c1"), interrupt("c2")], turn(), 2).await.unwrap();
    assert_eq!(c.announce, [WakeMessage::resolved(S, t("g"))]);
    assert_eq!(status_of(&e, "g").await, TurnStatus::Ready);
}

#[tokio::test]
async fn a_commit_acting_on_a_call_already_closed_finds_the_work_done() {
    let e = engine();
    e.park(&session(), call("a"), turn(), 1).await.unwrap();
    e.cancel(&t("a"), turn(), 2).await.unwrap();
    // The cancel in the commit has nothing left to do, and is not an error.
    let c = e.commit(&session(), vec![park("b"), cancel("a")], turn(), 3).await.unwrap();
    assert_eq!(c.result.parked.len(), 1);
    assert_eq!(status_of(&e, "b").await, TurnStatus::Parked);
}

// ---- when something goes wrong in the middle ------------------------------------------------------------------------

/// A store that takes a correlation for a ticket nobody here made, the moment a commit's intent
/// is written: another session winning the race in the gap between validation and application.
struct Racy {
    inner: MemParkStore,
    steal: &'static str,
}

/// A store that stops accepting writes the moment a commit's intent is written: the caller that
/// wrote the intent dies before it applies anything.
struct DiesAfterIntent {
    inner: Arc<MemParkStore>,
    dead: Arc<AtomicBool>,
}

macro_rules! store_for {
    ($ty:ty, $inner:ident, $dead:expr) => {
        impl ParkStore for $ty {
            async fn get(&self, t: &str) -> Result<Option<(Record, Revision)>> {
                self.$inner.get(t).await
            }
            async fn create(&self, t: &str, r: &Record) -> Result<bool> {
                $dead(self)?;
                self.$inner.create(t, r).await
            }
            async fn cas(&self, t: &str, e: Revision, r: &Record) -> Result<bool> {
                $dead(self)?;
                self.$inner.cas(t, e, r).await
            }
            async fn claim_correlation(&self, c: &str, t: &str) -> Result<Option<TicketId>> {
                $dead(self)?;
                self.$inner.claim_correlation(c, t).await
            }
            async fn find_correlation(&self, c: &str) -> Result<Option<TicketId>> {
                self.$inner.find_correlation(c).await
            }
            async fn index_session(&self, s: &str, t: &str) -> Result<()> {
                $dead(self)?;
                self.$inner.index_session(s, t).await
            }
            async fn list_session(&self, s: &str) -> Result<Vec<TicketId>> {
                self.$inner.list_session(s).await
            }
            async fn list_tickets(&self) -> Result<Vec<TicketId>> {
                self.$inner.list_tickets().await
            }
            async fn index_child(&self, p: &str, c: &str, max: u32) -> Result<IndexChild> {
                $dead(self)?;
                self.$inner.index_child(p, c, max).await
            }
            async fn list_children(&self, p: &str) -> Result<Vec<TicketId>> {
                self.$inner.list_children(p).await
            }
            async fn get_batch(&self, id: &str) -> Result<Option<(Batch, Revision)>> {
                self.$inner.get_batch(id).await
            }
            async fn cas_batch(&self, id: &str, e: Revision, b: &Batch) -> Result<bool> {
                $dead(self)?;
                self.$inner.cas_batch(id, e, b).await
            }
            async fn list_batches(&self) -> Result<Vec<String>> {
                self.$inner.list_batches().await
            }
            async fn create_batch(&self, id: &str, b: &Batch) -> Result<bool> {
                let made = self.$inner.create_batch(id, b).await?;
                self.after_intent().await;
                Ok(made)
            }
        }
    };
}

impl Racy {
    fn dead(&self) -> Result<()> {
        Ok(())
    }
    async fn after_intent(&self) {
        self.inner.claim_correlation(self.steal, "another-sessions-ticket").await.unwrap();
    }
}

impl DiesAfterIntent {
    fn dead(&self) -> Result<()> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(ParkError::storage("injected: the caller died after writing its intent"));
        }
        Ok(())
    }
    async fn after_intent(&self) {
        self.dead.store(true, Ordering::SeqCst);
    }
}

/// The storage that outlives one engine, for a second process to find what the first left.
struct Shared {
    inner: Arc<MemParkStore>,
}

impl Shared {
    fn dead(&self) -> Result<()> {
        Ok(())
    }
    async fn after_intent(&self) {}
}

store_for!(Shared, inner, |s: &Shared| s.dead());
store_for!(Racy, inner, |s: &Racy| s.dead());
store_for!(DiesAfterIntent, inner, |s: &DiesAfterIntent| s.dead());

#[tokio::test]
async fn a_park_refused_after_the_intent_was_written_rolls_back_what_the_commit_parked() {
    let e = Engine::new(Racy { inner: MemParkStore::new(), steal: "x" });
    e.park(&session(), call("existing"), turn(), 1).await.unwrap();
    e.park(&session(), call("old"), turn(), 1).await.unwrap();

    // Valid when it is checked; refused when it is applied, because "x" has been taken meanwhile.
    let r = e
        .commit(&session(), vec![park("a"), park("existing"), cancel("old"), park("x")], turn(), 10)
        .await;
    assert!(matches!(&r, Err(ParkError::Invalid(m)) if m.contains("aborted")), "{r:?}");

    // What the commit made is cancelled; what was already there is not the commit's to cancel;
    // and the cancel — which cannot be undone — was never applied, because parks go first.
    assert_eq!(status_of(&e, "a").await, TurnStatus::Cancelled);
    assert_eq!(status_of(&e, "existing").await, TurnStatus::Parked);
    assert_eq!(status_of(&e, "old").await, TurnStatus::Parked);

    // The commit is over. The same list again is refused, and nothing is parked a second time:
    // validation sees the correlation is taken before it ever looks at the aborted intent.
    let again = e
        .commit(&session(), vec![park("a"), park("existing"), cancel("old"), park("x")], turn(), 11)
        .await;
    assert!(matches!(&again, Err(ParkError::Invalid(_))), "{again:?}");
    assert_eq!(status_of(&e, "a").await, TurnStatus::Cancelled);
}

#[tokio::test]
async fn a_rolled_back_group_is_cancelled_children_first() {
    let e = Engine::new(Racy { inner: MemParkStore::new(), steal: "x" });
    let r = e
        .commit(
            &session(),
            vec![group("g", 2), child("c1", "g"), child("c2", "g"), park("x")],
            turn(),
            10,
        )
        .await;
    assert!(r.is_err());
    for c in ["g", "c1", "c2"] {
        assert_eq!(status_of(&e, c).await, TurnStatus::Cancelled, "{c}");
    }
}

#[tokio::test]
async fn a_commit_whose_caller_died_is_finished_by_a_pass() {
    let inner = Arc::new(MemParkStore::new());
    let dead = Arc::new(AtomicBool::new(false));
    let dying = Engine::new(DiesAfterIntent { inner: inner.clone(), dead: dead.clone() });
    let effects = || vec![group("g", 2), child("c1", "g"), child("c2", "g")];

    // The intent is written, and the caller is gone before any effect.
    let err = dying.commit(&session(), effects(), turn(), 1_000).await.unwrap_err();
    assert!(matches!(err, ParkError::Storage(_)), "{err:?}");
    assert!(
        dying.oplog(&session(), None, 10, 2_000).await.unwrap().is_empty(),
        "nothing applied yet"
    );

    // Another process finds the intent. It is too young to be anyone else's… until the lease runs out.
    let survivor = Engine::new(Shared { inner: inner.clone() });
    assert!(survivor.complete_batches(1_000, 1_500).await.unwrap().is_empty(), "inside the lease");
    let done = survivor.complete_batches(1_000, 2_000).await.unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].outcome, BatchOutcome::Completed);
    assert_eq!(parked_correlations(&survivor).await, ["c1", "c2", "g"]);

    // And it is done: a second pass has nothing, and the caller's own retry finds it so.
    assert!(survivor.complete_batches(0, 9_000).await.unwrap().is_empty());
    let retry = survivor.commit(&session(), effects(), turn(), 9_000).await.unwrap();
    assert!(retry.result.parked.iter().all(|p| p.outcome == ParkOutcome::AlreadyParked));
}

#[tokio::test]
async fn a_pass_that_cannot_finish_a_commit_rolls_it_back() {
    let inner = Arc::new(MemParkStore::new());
    let e = Engine::new(Shared { inner: inner.clone() });
    // A commit whose intent was written, and whose second park can never be applied: another
    // session already owns "x".
    e.park(&"other".to_string(), call("x"), turn(), 1).await.unwrap();
    let effects = vec![park("a"), park("x")];
    let id = batch_id(S, &effects);
    inner
        .create_batch(
            &id,
            &Batch {
                id: id.clone(),
                session: session(),
                effects,
                by: turn(),
                state: BatchState::Pending,
                created_at: 1_000,
                finished_at: None,
                reason: None,
                results: vec![],
            },
        )
        .await
        .unwrap();

    let done = e.complete_batches(0, 2_000).await.unwrap();
    assert_eq!(done.len(), 1);
    assert!(
        matches!(&done[0].outcome, BatchOutcome::Aborted(why) if why.contains("already in use")),
        "{done:?}"
    );
    assert_eq!(status_of(&e, "a").await, TurnStatus::Cancelled);
    assert!(e.complete_batches(0, 3_000).await.unwrap().is_empty(), "aborted is finished");
}

// ---- the sweep -----------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_sweep_finishes_commits_and_announces_what_they_resolved() {
    use holon_park::recover::RecoveryPolicy;
    use holon_park::resume::{sweep, Announcer};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Said(Mutex<Vec<WakeMessage>>);
    impl Announcer for Said {
        async fn announce(&self, m: &WakeMessage) -> Result<()> {
            self.0.lock().unwrap().push(m.clone());
            Ok(())
        }
    }

    let inner = Arc::new(MemParkStore::new());
    let e = Engine::new(Shared { inner: inner.clone() });
    e.commit(&session(), vec![group("g", 1), child("c", "g")], turn(), 500).await.unwrap();

    // A commit that answers the child, whose caller died before applying it.
    let effects = vec![wake("c")];
    let id = batch_id(S, &effects);
    inner
        .create_batch(
            &id,
            &Batch {
                id: id.clone(),
                session: session(),
                effects,
                by: turn(),
                state: BatchState::Pending,
                created_at: 1_000,
                finished_at: None,
                reason: None,
                results: vec![],
            },
        )
        .await
        .unwrap();

    let said = Said::default();
    let policy = RecoveryPolicy { batch_lease_ms: 0, ..RecoveryPolicy::default() };
    let swept = sweep(&e, &said, &Agent::named("comp-resume"), &policy, 2_000).await.unwrap();
    assert_eq!(swept.batches.len(), 1);
    assert_eq!(
        *said.0.lock().unwrap(),
        [WakeMessage::resolved(S, t("g"))],
        "the group, not the child"
    );
}
