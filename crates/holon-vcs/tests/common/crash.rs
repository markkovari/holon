//! Crash injection: every store behind one fuse that "kills the process" just
//! before its k-th write — that write and every call after it fail — then a
//! fresh engine repairs, and the invariants are checked. Run for every k from 1
//! to the number of writes the operation makes, for every kind of operation.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use holon_vcs::error::Result as VResult;
use holon_vcs::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use holon_vcs::model::*;
use holon_vcs::oplog::{NewOp, OpLog, OpState, StoredOp};
use holon_vcs::store::{BlobStore, CasMismatch, PointerStore, PointerValue, Revision};
use holon_vcs::{Engine, VcsError};

use super::*;

#[derive(Default)]
pub struct Fuse {
    writes: AtomicU32,
    /// Die before this write (1-based); 0: never.
    at: AtomicU32,
    dead: AtomicBool,
}

impl Fuse {
    pub fn arm(&self, at: u32) {
        self.writes.store(0, Ordering::SeqCst);
        self.at.store(at, Ordering::SeqCst);
        self.dead.store(false, Ordering::SeqCst);
    }
    pub fn writes(&self) -> u32 {
        self.writes.load(Ordering::SeqCst)
    }
    fn crashed() -> VcsError {
        VcsError::Storage("injected crash".into())
    }
    fn write(&self) -> VResult<()> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        let at = self.at.load(Ordering::SeqCst);
        if self.dead.load(Ordering::SeqCst) || (at != 0 && n >= at) {
            self.dead.store(true, Ordering::SeqCst);
            return Err(Self::crashed());
        }
        Ok(())
    }
    fn read(&self) -> VResult<()> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(Self::crashed());
        }
        Ok(())
    }
}

pub struct Crashy<T> {
    pub inner: Arc<T>,
    pub fuse: Arc<Fuse>,
}

impl<T: BlobStore> BlobStore for Crashy<T> {
    async fn put(&self, bytes: Vec<u8>) -> VResult<Hash> {
        self.fuse.write()?;
        self.inner.put(bytes).await
    }
    async fn get(&self, hash: &str) -> VResult<Option<Vec<u8>>> {
        self.fuse.read()?;
        self.inner.get(hash).await
    }
    async fn contains(&self, hash: &str) -> VResult<bool> {
        self.fuse.read()?;
        self.inner.contains(hash).await
    }
}

impl<T: PointerStore> PointerStore for Crashy<T> {
    async fn get(&self, key: &str) -> VResult<Option<(PointerValue, Revision)>> {
        self.fuse.read()?;
        self.inner.get(key).await
    }
    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: &PointerValue,
    ) -> VResult<Result<Revision, CasMismatch>> {
        self.fuse.write()?;
        self.inner.cas(key, expected, value).await
    }
}

impl<T: OpLog> OpLog for Crashy<T> {
    async fn append(&self, ws: &str, op: NewOp) -> VResult<StoredOp> {
        self.fuse.write()?;
        self.inner.append(ws, op).await
    }
    async fn get(&self, ws: &str, id: OpId) -> VResult<Option<StoredOp>> {
        self.fuse.read()?;
        self.inner.get(ws, id).await
    }
    async fn finish(&self, ws: &str, id: OpId, state: OpState) -> VResult<OpState> {
        self.fuse.write()?;
        self.inner.finish(ws, id, state).await
    }
    async fn list(&self, ws: &str, after: Option<OpId>, limit: u32) -> VResult<Vec<StoredOp>> {
        self.fuse.read()?;
        self.inner.list(ws, after, limit).await
    }
    async fn latest(&self, ws: &str) -> VResult<Option<OpId>> {
        self.fuse.read()?;
        self.inner.latest(ws).await
    }
    async fn settled(&self, ws: &str) -> VResult<OpId> {
        self.fuse.read()?;
        self.inner.settled(ws).await
    }
    async fn advance_settled(&self, ws: &str, to: OpId) -> VResult<()> {
        self.fuse.write()?;
        self.inner.advance_settled(ws, to).await
    }
}

impl<T: Graph> Graph for Crashy<T> {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> VResult<()> {
        self.fuse.write()?;
        self.inner.ensure_symbol(ws, key, id).await
    }
    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> VResult<()> {
        self.fuse.write()?;
        self.inner.update_symbol(ws, key, u).await
    }
    async fn symbol(&self, ws: &str, key: &str) -> VResult<Option<SymbolRecord>> {
        self.fuse.read()?;
        self.inner.symbol(ws, key).await
    }
    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> VResult<Vec<String>> {
        self.fuse.read()?;
        self.inner.symbols_named(ws, id).await
    }
    async fn symbols_in_component(&self, ws: &str, c: &str) -> VResult<Vec<SymbolRecord>> {
        self.fuse.read()?;
        self.inner.symbols_in_component(ws, c).await
    }
    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> VResult<()> {
        self.fuse.write()?;
        self.inner.put_patch(ws, p).await
    }
    async fn patch(&self, ws: &str, hash: &str) -> VResult<Option<PatchRecord>> {
        self.fuse.read()?;
        self.inner.patch(ws, hash).await
    }
    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> VResult<()> {
        self.fuse.write()?;
        self.inner.mark_patch(ws, hash, status, op, set_op).await
    }
    async fn dependents(&self, ws: &str, target: &SymbolId) -> VResult<Vec<Hash>> {
        self.fuse.read()?;
        self.inner.dependents(ws, target).await
    }
    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> VResult<()> {
        self.fuse.write()?;
        self.inner.put_conflict(ws, c, op).await
    }
    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> VResult<()> {
        self.fuse.write()?;
        self.inner.abandon_if_uncommitted(ws, id, op).await
    }
    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> VResult<()> {
        self.fuse.write()?;
        self.inner.apply_conflict_effect(ws, e, op).await
    }
    async fn conflict(&self, ws: &str, id: &str) -> VResult<Option<ConflictRecord>> {
        self.fuse.read()?;
        self.inner.conflict(ws, id).await
    }
    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> VResult<Vec<ConflictRecord>> {
        self.fuse.read()?;
        self.inner.conflicts(ws, state).await
    }
    async fn open_conflicts_for(&self, ws: &str, key: &str) -> VResult<Vec<ConflictRecord>> {
        self.fuse.read()?;
        self.inner.open_conflicts_for(ws, key).await
    }
}

pub type CrashEngine<B, P, G, L> = Engine<Crashy<B>, Crashy<P>, Crashy<G>, Crashy<L>>;

impl<B, P, G, L> Stores<B, P, G, L>
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    /// An engine whose every store is behind `fuse`.
    pub fn crashy(&self, fuse: &Arc<Fuse>) -> CrashEngine<B, P, G, L> {
        let c = |_: ()| fuse.clone();
        Engine::new(
            Crashy { inner: self.blobs.clone(), fuse: c(()) },
            Crashy { inner: self.pointers.clone(), fuse: c(()) },
            Crashy { inner: self.graph.clone(), fuse: c(()) },
            Crashy { inner: self.log.clone(), fuse: c(()) },
        )
    }
}

/// The operations crashed, one per kind of write sequence the engine has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// apply: replace (tip CAS only)
    Replace,
    /// apply: create (name claim + tip CAS)
    Create,
    /// apply: create with a placement
    CreatePlaced,
    /// apply: rename (claim + tip CAS + release)
    Rename,
    /// apply: delete (tip CAS + release)
    Delete,
    /// apply: a stale edit (conflict record + no-op CAS)
    Conflict,
    /// apply: move
    Move,
    /// resolve, with a sibling conflict re-pointed
    Resolve,
    /// resolve by deleting (tip CAS + name release)
    ResolveDelete,
    /// revert of a replace
    RevertReplace,
    /// revert of a rename (three pointers)
    RevertRename,
}

pub const CASES: &[Case] = &[
    Case::Replace,
    Case::Create,
    Case::CreatePlaced,
    Case::Rename,
    Case::Delete,
    Case::Conflict,
    Case::Move,
    Case::Resolve,
    Case::ResolveDelete,
    Case::RevertReplace,
    Case::RevertRename,
];

pub struct Ctx {
    a: SymbolId,
    b: SymbolId,
    pa: Hash,
    pb: Hash,
    cid: Option<ConflictId>,
    op: Option<OpId>,
}

async fn setup<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &Engine<B, P, G, L>,
    ws: &str,
    case: Case,
) -> Ctx {
    let (a, b) = (func("a"), func("b"));
    let pa = create(e, ws, &a, "fn a() {}\n").await;
    let pb = create(e, ws, &b, "fn b() {}\n").await;
    let mut ctx = Ctx { a: a.clone(), b, pa: pa.clone(), pb, cid: None, op: None };
    match case {
        Case::Conflict => {
            replace(e, ws, &a, &pa, "fn a() { 1 }\n", "setup").await.unwrap();
        }
        Case::Resolve | Case::ResolveDelete => {
            replace(e, ws, &a, &pa, "fn a() { 1 }\n", "w").await.unwrap();
            let c1 = replace(e, ws, &a, &pa, "fn a() { 2 }\n", "l1").await.unwrap();
            let c2 = replace(e, ws, &a, &pa, "fn a() { 3 }\n", "l2").await.unwrap();
            assert_eq!(c2.outcome, PatchOutcome::Conflicted);
            ctx.cid = c1.conflict;
        }
        Case::RevertReplace => {
            ctx.op = Some(replace(e, ws, &a, &pa, "fn a() { 1 }\n", "w").await.unwrap().op);
        }
        Case::RevertRename => {
            let r = e
                .apply_patch(req(ws, a, Some(pa), Transformation::Rename("a2".into()), "w"))
                .await
                .unwrap();
            ctx.op = Some(r.op);
        }
        _ => {}
    }
    ctx
}

/// What an operation did, to retry it and count it.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Commit(CommitResult),
    Reverted(OpEntry),
}

async fn run<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &Engine<B, P, G, L>,
    ws: &str,
    case: Case,
    ctx: &Ctx,
) -> Result<Done, VcsError> {
    let c = func("c");
    let resolve = |t: Transformation| ResolutionRequest {
        workspace: ws.to_string(),
        conflict: ctx.cid.clone().unwrap(),
        resolution: t,
        agent: Agent::named("resolver"),
        message: None,
    };
    let commit = match case {
        Case::Replace => replace(e, ws, &ctx.a, &ctx.pa, "fn a() { 1 }\n", "x").await,
        Case::Create => {
            e.apply_patch(req(ws, c, None, Transformation::Create(inline("fn c() {}\n")), "x"))
                .await
        }
        Case::CreatePlaced => {
            let mut r = req(ws, c, None, Transformation::Create(inline("fn c() {}\n")), "x");
            r.position = Some(Placement::After(ctx.a.clone()));
            e.apply_patch(r).await
        }
        Case::Rename => {
            e.apply_patch(req(
                ws,
                ctx.a.clone(),
                Some(ctx.pa.clone()),
                Transformation::Rename("a2".into()),
                "x",
            ))
            .await
        }
        Case::Delete => {
            e.apply_patch(req(ws, ctx.b.clone(), Some(ctx.pb.clone()), Transformation::Delete, "x"))
                .await
        }
        Case::Conflict => replace(e, ws, &ctx.a, &ctx.pa, "fn a() { stale }\n", "x").await,
        Case::Move => {
            e.apply_patch(req(
                ws,
                ctx.a.clone(),
                Some(ctx.pa.clone()),
                Transformation::Move(Placement::Last),
                "x",
            ))
            .await
        }
        Case::Resolve => {
            e.resolve_conflict(resolve(Transformation::Replace(inline("fn a() { 1 + 2 }\n")))).await
        }
        Case::ResolveDelete => e.resolve_conflict(resolve(Transformation::Delete)).await,
        Case::RevertReplace | Case::RevertRename => {
            return e
                .revert_op(ws, ctx.op.unwrap(), Agent::named("undo"))
                .await
                .map(Done::Reverted);
        }
    };
    commit.map(Done::Commit)
}

/// How many committed ops did what `done` did.
async fn times_done<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &Engine<B, P, G, L>,
    ws: &str,
    case: Case,
    ctx: &Ctx,
    done: &Done,
) -> usize {
    let ops = e.oplog(ws, None, 10_000).await.unwrap();
    ops.iter()
        .filter(|o| match (done, &o.kind, case) {
            (_, OpKind::Resolve(c), Case::Resolve | Case::ResolveDelete) => {
                Some(c) == ctx.cid.as_ref()
            }
            (Done::Commit(r), OpKind::Apply(h), _) => *h == r.patch,
            (_, OpKind::Revert(o), Case::RevertReplace | Case::RevertRename) => Some(*o) == ctx.op,
            _ => false,
        })
        .count()
}

/// The state the operation, done exactly once, leaves.
async fn check_effect<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &Engine<B, P, G, L>,
    ws: &str,
    case: Case,
    ctx: &Ctx,
) {
    let c = func("c");
    let a2 = func("a2");
    let text = |snap: Snapshot| async move { file_text(e, &snap, FILE).await };
    match case {
        Case::Replace => assert_eq!(content_of(e, ws, &ctx.a).await, "fn a() { 1 }\n"),
        Case::Create => assert_eq!(content_of(e, ws, &c).await, "fn c() {}\n"),
        Case::CreatePlaced => {
            let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
            assert_eq!(text(snap).await, "fn a() {}\nfn c() {}\nfn b() {}\n");
        }
        Case::Rename => {
            assert_eq!(tip_of(e, ws, &ctx.a).await, None);
            assert_eq!(content_of(e, ws, &a2).await, "fn a() {}\n");
        }
        Case::Delete => assert_eq!(tip_of(e, ws, &ctx.b).await, None),
        Case::Conflict => {
            let open = e.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
            assert_eq!(open.len(), 1, "{open:?}");
            assert_eq!(open[0].right.content, Some(inline("fn a() { stale }\n")));
        }
        Case::Move => {
            let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
            assert_eq!(text(snap).await, "fn b() {}\nfn a() {}\n");
        }
        Case::Resolve | Case::ResolveDelete => {
            let all = e.list_conflicts(ws, None).await.unwrap();
            let c1 = all.iter().find(|c| Some(&c.id) == ctx.cid.as_ref()).unwrap();
            assert_eq!(c1.state, ConflictState::Resolved);
            // The sibling was re-pointed at the resolution: one open, against it.
            let open: Vec<_> = all.iter().filter(|c| c.state == ConflictState::Open).collect();
            assert_eq!(open.len(), 1, "{all:?}");
            assert_eq!(Some(&open[0].left.patch), c1.resolved_by.as_ref());
            if case == Case::Resolve {
                assert_eq!(content_of(e, ws, &ctx.a).await, "fn a() { 1 + 2 }\n");
            } else {
                assert_eq!(tip_of(e, ws, &ctx.a).await, None);
            }
        }
        Case::RevertReplace => assert_eq!(content_of(e, ws, &ctx.a).await, "fn a() {}\n"),
        Case::RevertRename => {
            assert_eq!(content_of(e, ws, &ctx.a).await, "fn a() {}\n");
            assert_eq!(tip_of(e, ws, &a2).await, None);
        }
    }
}

/// Every invariant the stores must satisfy between operations.
pub async fn assert_consistent<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &Engine<B, P, G, L>,
    ws: &str,
) {
    let report = e.verify(ws).await.unwrap();
    assert!(report.issues.is_empty(), "{ws}: {:#?}", report.issues);
    assert!(report.in_flight.is_empty(), "{ws}: in flight {:?}", report.in_flight);
    // Nothing pending: the settled head is the end of the log.
    assert_eq!(
        Some(e.oplog_head(ws).await.unwrap()).filter(|h| *h > 0),
        e.log().latest(ws).await.unwrap()
    );
    // Every open conflict is committed and against its symbol's current tip.
    for c in e.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap() {
        assert_ne!(c.opened_at, 0, "{c:?}");
        let key = e.graph().conflict(ws, &c.id).await.unwrap().unwrap().key;
        let tip = e.pointers().get(&symbol_pointer(ws, &key)).await.unwrap().unwrap().0.value;
        assert_eq!(tip.as_ref(), Some(&c.left.patch), "{c:?}");
    }
}

/// Crash `case` before each of its writes in turn; repair (and, with `lazy`,
/// first retry WITHOUT repairing, so the readers' own settling is what
/// recovers); check. Returns the number of crash points run.
pub async fn crash_every_step<B, P, G, L>(
    s: Stores<B, P, G, L>,
    ws: &str,
    case: Case,
    lazy: bool,
) -> u32
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    // Lease 0: a pending op is presumed dead at once — the crashed process is.
    let clean = s.engine_owned().with_lease_ms(0);
    let fuse = Arc::new(Fuse::default());
    let crashy = s.crashy(&fuse);

    // How many writes the operation makes when nothing crashes.
    let dry = format!("{ws}/dry");
    let ctx = setup(&clean, &dry, case).await;
    fuse.arm(0);
    run(&crashy, &dry, case, &ctx).await.unwrap_or_else(|e| panic!("{case:?}: {e}"));
    let steps = fuse.writes();
    assert!(steps >= 3, "{case:?} made {steps} writes");
    check_effect(&clean, &dry, case, &ctx).await;
    assert_consistent(&clean, &dry).await;

    for k in 1..=steps {
        let ws = format!("{ws}/k{k}");
        let ctx = setup(&clean, &ws, case).await;
        fuse.arm(k);
        let crashed = run(&crashy, &ws, case, &ctx).await;
        assert!(
            crashed.is_err(),
            "{case:?} at step {k}/{steps} did not see the crash: {crashed:?}"
        );

        if lazy {
            let retry = run(&clean, &ws, case, &ctx).await;
            check_retry(case, k, &retry);
        }
        let report = clean.repair(&ws).await.unwrap();
        assert!(report.remaining.is_empty(), "{case:?} at {k}/{steps}: {report:#?}");
        assert_consistent(&clean, &ws).await;

        // Retrying the operation: exactly one op did it, however far the crashed
        // attempt got.
        let retry = run(&clean, &ws, case, &ctx).await;
        check_retry(case, k, &retry);
        let done = match retry {
            Ok(d) => d,
            // A revert that had landed refuses the second time: find it instead.
            Err(_) => {
                let ops = clean.oplog(&ws, None, 10_000).await.unwrap();
                Done::Reverted(
                    ops.into_iter()
                        .rev()
                        .find(|o| o.kind == OpKind::Revert(ctx.op.unwrap()))
                        .unwrap(),
                )
            }
        };
        assert_eq!(times_done(&clean, &ws, case, &ctx, &done).await, 1, "{case:?} at {k}/{steps}");
        check_effect(&clean, &ws, case, &ctx).await;
        assert_consistent(&clean, &ws).await;

        // And the workspace keeps working.
        let later = create(&clean, &ws, &func("later"), "fn later() {}\n").await;
        replace(&clean, &ws, &func("later"), &later, "fn later() { 1 }\n", "y").await.unwrap();
        assert_consistent(&clean, &ws).await;
    }
    steps
}

fn check_retry(case: Case, k: u32, retry: &Result<Done, VcsError>) {
    match (case, retry) {
        (Case::RevertReplace | Case::RevertRename, Err(VcsError::ConcurrentModification(_))) => {}
        (Case::Conflict, Ok(Done::Commit(r))) => {
            assert_eq!(r.outcome, PatchOutcome::Conflicted, "{case:?} at {k}: {r:?}")
        }
        (_, Ok(Done::Commit(r))) => assert!(
            matches!(
                r.outcome,
                PatchOutcome::Applied | PatchOutcome::Commuted | PatchOutcome::Duplicate
            ),
            "{case:?} at {k}: {r:?}"
        ),
        (_, Ok(Done::Reverted(_))) => {}
        (_, Err(e)) => panic!("{case:?} at step {k}: retry failed: {e}"),
    }
}
