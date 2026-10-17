//! Crash injection, the technique of `holon-vcs`'s `tests/common/crash.rs`: every store
//! operation behind one fuse that "kills the process" at the k-th write, then a fresh engine
//! over the same storage repairs, and the invariants are checked. Run for every k from 1 to
//! the number of writes the operation makes, for every kind of operation, in two modes:
//!
//! * **before** — the k-th write never happens (the process died first);
//! * **after** — the k-th write lands and the caller never learns it (the reply was lost, the
//!   process died before it could act). `holon-vcs` injects the first; against a networked
//!   store the second is as common, and it is the one that produces a duplicate, or an
//!   announcement nobody made.
//!
//! Once the fuse is dead every read and write fails, like a process that is gone. The "restart"
//! is an engine with no fuse over the same storage.
//!
//! The matrix runs twice: over `MemParkStore`, always, and over a real NATS JetStream KV when
//! `HOLON_PARK_NATS_URL` is set (`--features native`), where the adapter's own read-modify-write
//! paths — the session list, the correlation claim, the key scan — are what is crashed.
//!
//! What must hold after any crash, once the caller has retried and a repair pass has run:
//!
//! * every ticket is coherent (its status and its fields agree) and reachable by every index:
//!   its session's list, and the `correlation` a webhook will present;
//! * the operation happened exactly once, however far the crashed attempt got;
//! * the workspace keeps working: a new call parks, wakes and is taken.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use std::future::Future;

use holon_park::error::{ParkError, Result};
use holon_park::group::{join_decision, Join};
use holon_park::mem::MemParkStore;
use holon_park::model::{
    Agent, CallResult, ChildResult, GroupSpec, JoinOutcome, JoinPolicy, OutboundCall, ReplayPolicy,
    Resolution, SessionId, TicketId, TurnStatus, WakeMessage,
};
use holon_park::recover::RecoveryPolicy;
use holon_park::resume::{handle, Delivery, Sink, SinkReply};
use holon_park::store::{IndexChild, ParkStore, Record, Revision, StoredStatus};
use holon_park::ticket::ticket_id;
use holon_park::Engine;

// ---- the fuse ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Before,
    After,
}

#[derive(Default)]
struct Fuse {
    writes: AtomicU32,
    /// Die at this write (1-based); 0: never.
    at: AtomicU32,
    after: AtomicBool,
    dead: AtomicBool,
}

enum Fate {
    Go,
    Die,
    LandThenDie,
}

impl Fuse {
    fn arm(&self, at: u32, mode: Mode) {
        self.writes.store(0, Ordering::SeqCst);
        self.at.store(at, Ordering::SeqCst);
        self.after.store(mode == Mode::After, Ordering::SeqCst);
        self.dead.store(false, Ordering::SeqCst);
    }

    fn writes(&self) -> u32 {
        self.writes.load(Ordering::SeqCst)
    }

    fn write(&self) -> Fate {
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        if self.dead.load(Ordering::SeqCst) {
            return Fate::Die;
        }
        if self.at.load(Ordering::SeqCst) == n {
            self.dead.store(true, Ordering::SeqCst);
            return if self.after.load(Ordering::SeqCst) { Fate::LandThenDie } else { Fate::Die };
        }
        Fate::Go
    }

    fn read(&self) -> Result<()> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(crashed());
        }
        Ok(())
    }
}

fn crashed() -> ParkError {
    ParkError::Storage("injected crash".into())
}

/// A store that can be made fresh, once per crash point.
trait Backend: Send + Sync + 'static {
    type Store: ParkStore + 'static;
    fn fresh(&self) -> impl Future<Output = Self::Store> + Send;
}

struct Mem;

impl Backend for Mem {
    type Store = MemParkStore;
    async fn fresh(&self) -> MemParkStore {
        MemParkStore::new()
    }
}

/// The storage that survives the crash, handed to each engine in turn.
struct Shared<S>(Arc<S>);

/// The same storage behind the fuse.
struct Crashy<S> {
    inner: Arc<S>,
    fuse: Arc<Fuse>,
}

macro_rules! reads {
    ($gate:expr) => {
        async fn get(&self, t: &str) -> Result<Option<(Record, Revision)>> {
            $gate(self)?;
            self.inner().get(t).await
        }
        async fn find_correlation(&self, c: &str) -> Result<Option<TicketId>> {
            $gate(self)?;
            self.inner().find_correlation(c).await
        }
        async fn list_session(&self, s: &str) -> Result<Vec<TicketId>> {
            $gate(self)?;
            self.inner().list_session(s).await
        }
        async fn list_tickets(&self) -> Result<Vec<TicketId>> {
            $gate(self)?;
            self.inner().list_tickets().await
        }
        async fn list_children(&self, p: &str) -> Result<Vec<TicketId>> {
            $gate(self)?;
            self.inner().list_children(p).await
        }
    };
}

impl<S: ParkStore> Shared<S> {
    fn inner(&self) -> &S {
        &self.0
    }
}

impl<S: ParkStore> Crashy<S> {
    fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: ParkStore> ParkStore for Shared<S> {
    reads!(|_: &Self| Ok::<(), ParkError>(()));
    async fn create(&self, t: &str, r: &Record) -> Result<bool> {
        self.inner().create(t, r).await
    }
    async fn cas(&self, t: &str, e: Revision, r: &Record) -> Result<bool> {
        self.inner().cas(t, e, r).await
    }
    async fn claim_correlation(&self, c: &str, t: &str) -> Result<Option<TicketId>> {
        self.inner().claim_correlation(c, t).await
    }
    async fn index_session(&self, s: &str, t: &str) -> Result<()> {
        self.inner().index_session(s, t).await
    }
    async fn index_child(&self, p: &str, c: &str, max: u32) -> Result<IndexChild> {
        self.inner().index_child(p, c, max).await
    }
}

/// One write: apply it, or not, or apply it and then lose the reply.
macro_rules! write {
    ($self:ident, $call:expr) => {
        match $self.fuse.write() {
            Fate::Go => $call.await,
            Fate::Die => Err(crashed()),
            Fate::LandThenDie => {
                let _ = $call.await;
                Err(crashed())
            }
        }
    };
}

impl<S: ParkStore> ParkStore for Crashy<S> {
    reads!(|this: &Self| this.fuse.read());
    async fn create(&self, t: &str, r: &Record) -> Result<bool> {
        write!(self, self.inner().create(t, r))
    }
    async fn cas(&self, t: &str, e: Revision, r: &Record) -> Result<bool> {
        write!(self, self.inner().cas(t, e, r))
    }
    async fn claim_correlation(&self, c: &str, t: &str) -> Result<Option<TicketId>> {
        write!(self, self.inner().claim_correlation(c, t))
    }
    async fn index_session(&self, s: &str, t: &str) -> Result<()> {
        write!(self, self.inner().index_session(s, t))
    }
    async fn index_child(&self, p: &str, c: &str, max: u32) -> Result<IndexChild> {
        write!(self, self.inner().index_child(p, c, max))
    }
}

struct World<S> {
    inner: Arc<S>,
    fuse: Arc<Fuse>,
}

impl<S: ParkStore + 'static> World<S> {
    async fn new<B: Backend<Store = S>>(b: &B) -> World<S> {
        World { inner: Arc::new(b.fresh().await), fuse: Arc::new(Fuse::default()) }
    }
    /// The restarted process: no fuse.
    fn clean(&self) -> Engine<Shared<S>> {
        Engine::new(Shared(self.inner.clone()))
    }
    fn crashy(&self) -> Engine<Crashy<S>> {
        Engine::new(Crashy { inner: self.inner.clone(), fuse: self.fuse.clone() })
    }
}

// ---- what is crashed -------------------------------------------------------------

const PARKED_AT: u64 = 1_000;
const DEADLINE: u64 = 2_000;
/// Far past every deadline and every re-announce gap below.
const NOW: u64 = 100_000;
const SESSION: &str = "s1";

fn policy() -> RecoveryPolicy {
    RecoveryPolicy {
        deadline_grace_ms: 0,
        max_redispatches: 3,
        retry_after_ms: 1_000,
        reannounce_after_ms: 500,
    }
}

fn sweeper() -> Agent {
    Agent::named("comp-resume")
}

fn session() -> SessionId {
    SESSION.to_string()
}

fn call(correlation: &str, replay: ReplayPolicy, deadline: Option<u64>) -> OutboundCall {
    OutboundCall {
        correlation: correlation.into(),
        description: format!("call {correlation}"),
        deadline,
        poll: None,
        replay,
        parent: None,
        group: None,
    }
}

fn tk(correlation: &str) -> TicketId {
    ticket_id(SESSION, correlation)
}

fn group_call(policy: JoinPolicy, children: u32, deadline: Option<u64>) -> OutboundCall {
    OutboundCall {
        group: Some(GroupSpec { policy, children }),
        ..call("g", ReplayPolicy::Unsafe, deadline)
    }
}

fn child_call(correlation: &str) -> OutboundCall {
    OutboundCall { parent: Some("g".into()), ..call(correlation, ReplayPolicy::Unsafe, None) }
}

/// A group, and `children` of its children, each answered or not as the case needs.
async fn park_group<S: ParkStore>(
    e: &Engine<S>,
    policy: JoinPolicy,
    declared: u32,
    deadline: Option<u64>,
    children: &[&str],
) -> Vec<TicketId> {
    let turn = || Agent::named("turn");
    let mut out = vec![
        e.park(&session(), group_call(policy, declared, deadline), turn(), PARKED_AT)
            .await
            .unwrap()
            .ticket,
    ];
    for (i, c) in children.iter().enumerate() {
        out.push(
            e.park(&session(), child_call(c), turn(), PARKED_AT + 1 + i as u64)
                .await
                .unwrap()
                .ticket,
        );
    }
    out
}

fn answer() -> CallResult {
    CallResult { ok: true, body: b"42".to_vec(), detail: None }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    /// create + session index + correlation claim
    Park,
    /// one CAS, and a nudge the caller never gets to publish
    Wake,
    Interrupt,
    /// at most once, by design: a taker that dies after the take has lost the answer
    TakeReady,
    Cancel,
    /// two interruptions in one pass
    RecoverUnsafe,
    /// two grants in one pass
    RecoverSafe,
    /// an answer nobody took
    Reannounce,
    /// the resumer: deliver to the sink, then take
    Deliver,
    /// a child: its place in the group, then the record, then three indexes
    ParkChild,
    /// the last child answers: its own write, the group's resolution, the children closed
    JoinAll,
    /// a failure under fail-fast: the siblings cancelled first, then the group, then the rest
    FailFast,
    /// bottom-up: the children, then the group
    CancelGroup,
    /// a group's own deadline: the children still parked cancelled, then the group
    GroupTimeout,
}

const CASES: &[Case] = &[
    Case::Park,
    Case::Wake,
    Case::Interrupt,
    Case::TakeReady,
    Case::Cancel,
    Case::RecoverUnsafe,
    Case::RecoverSafe,
    Case::Reannounce,
    Case::Deliver,
    Case::ParkChild,
    Case::JoinAll,
    Case::FailFast,
    Case::CancelGroup,
    Case::GroupTimeout,
];

/// What every attempt at a case shares: its sink, which outlives a crash.
#[derive(Default)]
struct Recorder(Mutex<Vec<Delivery>>);

impl Sink for Recorder {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        self.0.lock().unwrap().push(d.clone());
        SinkReply::Delivered
    }
}

struct Ctx {
    /// The tickets the case is about.
    tickets: Vec<TicketId>,
    sink: Recorder,
}

async fn park_one<S: ParkStore>(
    e: &Engine<S>,
    correlation: &str,
    replay: ReplayPolicy,
    d: Option<u64>,
) -> TicketId {
    e.park(&session(), call(correlation, replay, d), Agent::named("turn"), PARKED_AT)
        .await
        .unwrap()
        .ticket
}

async fn setup<S: ParkStore>(e: &Engine<S>, case: Case) -> Ctx {
    let mut tickets = Vec::new();
    match case {
        Case::Park => {}
        Case::Wake | Case::Interrupt | Case::Cancel => {
            tickets.push(park_one(e, "c1", ReplayPolicy::Unsafe, None).await);
        }
        Case::TakeReady | Case::Reannounce | Case::Deliver => {
            tickets.push(park_one(e, "c1", ReplayPolicy::Unsafe, None).await);
            e.wake("c1", answer(), PARKED_AT + 1).await.unwrap();
        }
        Case::RecoverUnsafe => {
            for c in ["c1", "c2"] {
                tickets.push(park_one(e, c, ReplayPolicy::Unsafe, Some(DEADLINE)).await);
            }
        }
        Case::RecoverSafe => {
            for c in ["c1", "c2"] {
                tickets.push(park_one(e, c, ReplayPolicy::Safe, Some(DEADLINE)).await);
            }
        }
        Case::ParkChild => {
            tickets = park_group(e, JoinPolicy::AllSettled, 2, None, &[]).await;
        }
        Case::JoinAll | Case::CancelGroup => {
            tickets = park_group(e, JoinPolicy::AllSettled, 2, None, &["c1", "c2"]).await;
            e.wake("c1", answer(), PARKED_AT + 10).await.unwrap();
        }
        Case::FailFast => {
            tickets = park_group(e, JoinPolicy::FailFast, 3, None, &["c1", "c2", "c3"]).await;
        }
        Case::GroupTimeout => {
            tickets = park_group(e, JoinPolicy::AllSettled, 3, Some(DEADLINE), &["c1", "c2"]).await;
            e.wake("c1", answer(), PARKED_AT + 10).await.unwrap();
        }
    }
    Ctx { tickets, sink: Recorder::default() }
}

/// What an attempt did, so a retry can be told from a first try.
#[derive(Debug)]
enum Done {
    Parked,
    Woken {
        fresh: bool,
    },
    Interrupted,
    Taken(Resolution),
    Cancelled,
    Recovered,
    Delivered,
    /// What the call asked to be announced on `PARK_WAKE`.
    Announced(Vec<WakeMessage>),
}

async fn run<S: ParkStore>(e: &Engine<S>, case: Case, ctx: &Ctx) -> Result<Done> {
    let t0 = ctx.tickets.first().cloned().unwrap_or_default();
    Ok(match case {
        Case::Park => {
            e.park(
                &session(),
                call("c1", ReplayPolicy::Unsafe, None),
                Agent::named("turn"),
                PARKED_AT,
            )
            .await?;
            Done::Parked
        }
        Case::Wake => Done::Woken { fresh: e.wake("c1", answer(), NOW).await?.freshly_woken },
        Case::Interrupt => {
            e.interrupt(&t0, "restarted mid-call", Agent::named("turn"), NOW).await?;
            Done::Interrupted
        }
        Case::TakeReady => Done::Taken(e.take_ready(&t0, NOW).await?),
        Case::Cancel => {
            e.cancel(&t0, Agent::named("turn"), NOW).await?;
            Done::Cancelled
        }
        Case::RecoverUnsafe | Case::RecoverSafe | Case::Reannounce => {
            e.recover(&sweeper(), &policy(), NOW).await?;
            Done::Recovered
        }
        Case::Deliver => {
            handle(e, &ctx.sink, &WakeMessage::resolved(SESSION, &t0), &sweeper(), NOW).await?;
            Done::Delivered
        }
        Case::ParkChild => {
            e.park(&session(), child_call("c1"), Agent::named("turn"), PARKED_AT + 1).await?;
            Done::Parked
        }
        Case::JoinAll => Done::Announced(e.wake("c2", answer(), NOW).await?.announce),
        Case::FailFast => Done::Announced(
            e.interrupt(&tk("c1"), "restarted mid-call", Agent::named("turn"), NOW).await?.announce,
        ),
        Case::CancelGroup => {
            e.cancel(&tk("g"), Agent::named("turn"), NOW).await?;
            Done::Cancelled
        }
        Case::GroupTimeout => {
            e.recover(&sweeper(), &policy(), NOW).await?;
            Done::Recovered
        }
    })
}

async fn record_of<S: ParkStore>(w: &World<S>, ticket: &TicketId) -> Record {
    let (r, _) =
        Shared(w.inner.clone()).get(ticket).await.unwrap().expect("a ticket the case made is gone");
    r
}

/// The repair pass a restarted process runs.
async fn heal<S: ParkStore>(e: &Engine<S>) -> Result<()> {
    e.repair().await.map(|_| ())
}

/// A call that resolved a group says so, and if its attempt that did was the crashed one the
/// announcement was lost: the re-announce row has to find it.
async fn check_announced<S: ParkStore>(e: &Engine<S>, retry: &Result<Done>, group: &TicketId) {
    match retry {
        Ok(Done::Announced(a)) if a.contains(&WakeMessage::resolved(SESSION, group)) => {}
        // The call found its work already done: the crashed attempt got as far as consuming the
        // child, and a retry on a closed child is told so. The announcement it would have made
        // died with it, which is the same case.
        Ok(Done::Announced(_)) | Err(ParkError::AlreadyClosed(_)) => {
            if let Ok(Done::Announced(a)) = retry {
                assert!(a.is_empty(), "announced something else: {a:?}");
            }
            let swept = e.recover(&sweeper(), &policy(), NOW + 10_000).await.unwrap();
            assert!(
                swept.iter().any(|r| &r.ticket == group && r.announcement().is_some()),
                "a resolved group nobody announced must be found: {swept:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// The state the operation, done exactly once, leaves.
async fn check_effect<S: ParkStore>(
    w: &World<S>,
    e: &Engine<Shared<S>>,
    case: Case,
    ctx: &Ctx,
    retry: &Result<Done>,
) {
    match case {
        Case::Park => {
            let pending = e.pending(&session(), NOW).await.unwrap();
            assert_eq!(pending.len(), 1, "exactly one ticket: {pending:?}");
            assert_eq!(pending[0].status, TurnStatus::Parked);
        }
        Case::Wake => {
            let r = record_of(w, &ctx.tickets[0]).await;
            assert_eq!(r.status, StoredStatus::Ready);
            assert_eq!(r.result, Some(answer()));
            // If the retry found it already woken, nobody told a resumer: that is what the
            // re-announce row exists for, and it has to find it.
            if matches!(retry, Ok(Done::Woken { fresh: false })) {
                let swept = e.recover(&sweeper(), &policy(), NOW + 10_000).await.unwrap();
                assert_eq!(swept.len(), 1, "an answer nobody announced must be found: {swept:?}");
            }
        }
        Case::Interrupt => {
            let r = record_of(w, &ctx.tickets[0]).await;
            assert_eq!(r.status, StoredStatus::Interrupted);
            assert_eq!(r.interruption.unwrap().reason, "restarted mid-call");
        }
        Case::TakeReady => {
            let r = record_of(w, &ctx.tickets[0]).await;
            assert_eq!(r.status, StoredStatus::Resumed);
            // At most once, by design: a retry either takes it or is told it is gone. The
            // answer a crashed taker never saw is not recoverable through `take-ready`, which
            // is why the resumer reads before it takes.
            match retry {
                Ok(Done::Taken(Resolution::Answered(a))) => assert_eq!(a, &answer()),
                Err(ParkError::AlreadyClosed(_)) => {}
                other => panic!("{other:?}"),
            }
        }
        Case::Cancel => {
            assert_eq!(record_of(w, &ctx.tickets[0]).await.status, StoredStatus::Cancelled);
        }
        Case::RecoverUnsafe => {
            for t in &ctx.tickets {
                let r = record_of(w, t).await;
                assert_eq!(r.status, StoredStatus::Interrupted, "{t}");
                assert_eq!(r.interruption.unwrap().at, NOW);
            }
        }
        Case::RecoverSafe => {
            for t in &ctx.tickets {
                let r = record_of(w, t).await;
                assert_eq!(r.status, StoredStatus::Parked);
                assert_eq!(
                    r.redispatches, 1,
                    "granted once, however far the crashed pass got: {t}"
                );
                assert_eq!(r.call.deadline, Some(NOW + policy().retry_after_ms));
            }
        }
        Case::Reannounce => {
            let r = record_of(w, &ctx.tickets[0]).await;
            assert_eq!(r.status, StoredStatus::Ready);
            assert_eq!(r.announced_at, Some(NOW));
        }
        Case::ParkChild => {
            let r = record_of(w, &tk("c1")).await;
            assert_eq!(r.call.parent.as_deref(), Some("g"));
            let kids = Shared(w.inner.clone()).list_children(&tk("g")).await.unwrap();
            assert_eq!(kids, [tk("c1")], "listed once, however far the crashed attempt got");
            // The places were counted right: one more fits, and the one after does not.
            let turn = || Agent::named("turn");
            e.park(&session(), child_call("c2"), turn(), NOW).await.unwrap();
            let full = e.park(&session(), child_call("c3"), turn(), NOW).await;
            assert!(matches!(full, Err(ParkError::Invalid(_))), "{full:?}");
        }
        Case::JoinAll => {
            let g = record_of(w, &tk("g")).await;
            assert_eq!(g.status, StoredStatus::Ready);
            let j = g.joined.expect("a resolved group holds its result");
            assert_eq!((j.outcome, j.children.len(), j.missing), (JoinOutcome::AllAnswered, 2, 0));
            for c in ["c1", "c2"] {
                assert_eq!(record_of(w, &tk(c)).await.status, StoredStatus::Resumed, "{c}");
            }
            check_announced(e, retry, &tk("g")).await;
        }
        Case::FailFast => {
            let g = record_of(w, &tk("g")).await;
            assert_eq!(g.status, StoredStatus::Ready);
            let j = g.joined.expect("a resolved group holds its result");
            assert_eq!(j.outcome, JoinOutcome::FailedFast);
            assert!(matches!(&j.children[0].ended, ChildResult::Interrupted(_)), "{j:?}");
            assert_eq!(j.children[1].ended, ChildResult::Cancelled);
            assert_eq!(j.children[2].ended, ChildResult::Cancelled);
            assert_eq!(record_of(w, &tk("c1")).await.status, StoredStatus::Resumed);
            for c in ["c2", "c3"] {
                assert_eq!(record_of(w, &tk(c)).await.status, StoredStatus::Cancelled, "{c}");
            }
            check_announced(e, retry, &tk("g")).await;
        }
        Case::CancelGroup => {
            for c in ["g", "c1", "c2"] {
                assert_eq!(record_of(w, &tk(c)).await.status, StoredStatus::Cancelled, "{c}");
            }
        }
        Case::GroupTimeout => {
            let g = record_of(w, &tk("g")).await;
            assert_eq!(g.status, StoredStatus::Ready);
            let j = g.joined.expect("a resolved group holds its result");
            assert_eq!((j.outcome, j.missing), (JoinOutcome::TimedOut, 1));
            assert!(matches!(&j.children[0].ended, ChildResult::Answered(_)), "{j:?}");
            assert_eq!(j.children[1].ended, ChildResult::Cancelled);
            assert_eq!(record_of(w, &tk("c1")).await.status, StoredStatus::Resumed);
            assert_eq!(record_of(w, &tk("c2")).await.status, StoredStatus::Cancelled);
        }
        Case::Deliver => {
            let r = record_of(w, &ctx.tickets[0]).await;
            assert_eq!(r.status, StoredStatus::Resumed);
            let seen = ctx.sink.0.lock().unwrap();
            assert!(!seen.is_empty(), "closed without the sink ever being handed it");
            assert!(
                seen.iter().all(|d| d.idempotency_key() == seen[0].idempotency_key()),
                "every delivery is the same delivery: {seen:?}"
            );
        }
    }
}

/// Every invariant the store must satisfy between operations.
async fn assert_consistent<S: ParkStore>(w: &World<S>, e: &Engine<Shared<S>>, what: &str) {
    let store = Shared(w.inner.clone());
    let tickets = store.list_tickets().await.unwrap();
    for t in &tickets {
        let (r, _) = store.get(t).await.unwrap().unwrap();
        let ctx = format!("{what}: ticket {t} ({:?})", r.status);
        match r.status {
            StoredStatus::Parked => {
                assert!(r.result.is_none() && r.interruption.is_none(), "{ctx}");
            }
            StoredStatus::Ready => {
                assert!((r.result.is_some() || r.joined.is_some()) && r.woken_at.is_some(), "{ctx}")
            }
            StoredStatus::Interrupted => assert!(r.interruption.is_some(), "{ctx}"),
            StoredStatus::Resumed => {
                assert!(r.resumed_at.is_some(), "{ctx}");
                assert!(
                    r.result.is_some() || r.interruption.is_some() || r.joined.is_some(),
                    "{ctx}"
                );
            }
            StoredStatus::Cancelled => assert!(r.cancelled_at.is_some(), "{ctx}"),
        }
        // Reachable the two ways a caller reaches a ticket it does not hold the id of.
        assert!(
            store.list_session(&r.session).await.unwrap().contains(t),
            "{ctx}: missing from its session's list, so `pending` and `oplog` cannot see it"
        );
        assert_eq!(
            store.find_correlation(&r.call.correlation).await.unwrap().as_deref(),
            Some(t.as_str()),
            "{ctx}: its correlation is not claimed, so a webhook's `wake` is refused"
        );
    }
    // And nothing the index names is missing.
    for t in store.list_session(&session()).await.unwrap() {
        assert!(tickets.contains(&t), "{what}: the session list names {t}, which does not exist");
    }

    // Groups (ADR-0109): a child is listed under its group, nothing a group lists is missing,
    // and a group's state agrees with its children's.
    for t in &tickets {
        let (r, _) = store.get(t).await.unwrap().unwrap();
        let ctx = format!("{what}: ticket {t} ({:?})", r.status);
        if let Some(parent) = &r.call.parent {
            let g = ticket_id(&r.session, parent);
            assert!(tickets.contains(&g), "{ctx}: its group does not exist");
            assert!(
                store.list_children(&g).await.unwrap().contains(t),
                "{ctx}: not listed under its group, so the group's join cannot see it"
            );
        }
        let Some(spec) = r.call.group else { continue };
        let ids = store.list_children(t).await.unwrap();
        let mut kids = Vec::new();
        for k in &ids {
            assert!(tickets.contains(k), "{ctx}: the group lists {k}, which does not exist");
            kids.push(store.get(k).await.unwrap().unwrap().0);
        }
        let over = |k: &Record| matches!(k.status, StoredStatus::Resumed | StoredStatus::Cancelled);
        match r.status {
            StoredStatus::Ready | StoredStatus::Resumed => {
                assert!(r.joined.is_some(), "{ctx}: resolved without a result");
                assert!(kids.iter().all(over), "{ctx}: resolved with a child still open: {kids:?}");
            }
            StoredStatus::Cancelled => {
                assert!(
                    kids.iter().all(over),
                    "{ctx}: cancelled with a child still open: {kids:?}"
                );
            }
            StoredStatus::Parked => {
                assert!(r.joined.is_none(), "{ctx}");
                assert_eq!(
                    join_decision(&spec, &kids),
                    Join::Wait,
                    "{ctx}: its join says resolve and it is still parked"
                );
            }
            StoredStatus::Interrupted => panic!("{ctx}: a group is never interrupted"),
        }
    }
    let _ = e;
}

/// One crash point: crash `case` at its `k`-th write, restart, retry and repair, check.
async fn one_point<B: Backend>(b: Arc<B>, case: Case, k: u32, steps: u32, mode: Mode, lazy: bool) {
    let what = format!("{case:?} at write {k}/{steps} ({mode:?}, lazy={lazy})");
    let w = World::new(&*b).await;
    let clean = w.clean();
    let ctx = setup(&clean, case).await;

    w.fuse.arm(k, mode);
    let crashed = run(&w.crashy(), case, &ctx).await;
    assert!(crashed.is_err(), "{what}: the crash was not seen: {crashed:?}");

    // The restarted process. `lazy`: the caller retries before anything has repaired;
    // otherwise a repair pass runs first.
    let restarted = w.clean();
    if !lazy {
        heal(&restarted).await.unwrap_or_else(|e| panic!("{what}: repair: {e}"));
    }
    // Nothing repairs after the retry: when `lazy`, the retry alone has to be enough.
    let retry = run(&restarted, case, &ctx).await;
    assert_consistent(&w, &restarted, &what).await;
    check_effect(&w, &restarted, case, &ctx, &retry).await;

    // And it keeps working: a new call parks, is answered, and is taken.
    let later = restarted
        .park(&session(), call("later", ReplayPolicy::Unsafe, None), Agent::named("t"), NOW)
        .await
        .unwrap_or_else(|e| panic!("{what}: afterwards: {e}"));
    restarted.wake("later", answer(), NOW).await.unwrap();
    assert!(matches!(
        restarted.take_ready(&later.ticket, NOW).await.unwrap(),
        Resolution::Answered(_)
    ));
    assert_consistent(&w, &restarted, &format!("{what}, afterwards")).await;
}

/// Crash `case` at each of its writes in turn, in both modes, retrying before and after a
/// repair. Returns the crash points run and the ones that failed, each with why.
async fn crash_every_step<B: Backend>(b: &Arc<B>, case: Case) -> (u32, Vec<String>) {
    // How many writes the operation makes when nothing crashes.
    let dry = World::new(&**b).await;
    let ctx = setup(&dry.clean(), case).await;
    dry.fuse.arm(0, Mode::Before);
    run(&dry.crashy(), case, &ctx).await.unwrap_or_else(|e| panic!("{case:?} dry run: {e}"));
    let steps = dry.fuse.writes();
    assert!(steps >= 1, "{case:?} made no writes");

    let (mut points, mut failed) = (0, Vec::new());
    for k in 1..=steps {
        for mode in [Mode::Before, Mode::After] {
            for lazy in [false, true] {
                points += 1;
                // A task of its own, so a failed assertion is reported and the rest still run.
                if let Err(e) = tokio::spawn(one_point(b.clone(), case, k, steps, mode, lazy)).await
                {
                    let msg = e.into_panic();
                    let msg = msg
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| msg.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    failed.push(msg);
                }
            }
        }
    }
    (points, failed)
}

async fn the_whole_matrix<B: Backend>(b: Arc<B>, what: &str) {
    let (mut points, mut failed) = (0, Vec::new());
    for &case in CASES {
        let (n, f) = crash_every_step(&b, case).await;
        eprintln!("{what}: crash {case:?}: {n} crash points, {} failed", f.len());
        points += n;
        failed.extend(f);
    }
    eprintln!("{what}: crash points covered: {points}");
    assert!(points >= 40, "the harness must actually run: {points}");
    assert!(
        failed.is_empty(),
        "{} of {points} crash points failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

#[tokio::test]
async fn crash_at_every_write_of_every_operation() {
    the_whole_matrix(Arc::new(Mem), "memory").await;
}

/// The same matrix over a real JetStream KV: what the adapter does between the fuse's writes (the
/// session list's read-modify-CAS, the correlation claim's create-conflict, the key scan) is real.
#[cfg(feature = "native")]
mod live {
    use super::*;
    use holon_park::nats::NatsParkStore;

    struct Nats {
        js: async_nats::jetstream::Context,
        tag: String,
        n: AtomicU32,
    }

    impl Backend for Nats {
        type Store = NatsParkStore;
        async fn fresh(&self) -> NatsParkStore {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            NatsParkStore::open(&self.js, &format!("holon-park-crash-{}-{n}", self.tag))
                .await
                .expect("a fresh bucket")
        }
    }

    #[tokio::test]
    async fn crash_at_every_write_of_every_operation_over_real_nats() {
        let Ok(url) = std::env::var("HOLON_PARK_NATS_URL") else {
            eprintln!(
                "SKIPPED crash_at_every_write_of_every_operation_over_real_nats: set \
                 HOLON_PARK_NATS_URL to run it against a real NATS JetStream"
            );
            return;
        };
        let js = async_nats::jetstream::new(async_nats::connect(&url).await.expect("connect"));
        let tag = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        );
        the_whole_matrix(Arc::new(Nats { js, tag, n: AtomicU32::new(0) }), "nats").await;
    }
}
