//! Scenarios, written once over any backends and run by `mem.rs` (always) and
//! `live.rs` (against NATS + SurrealDB, when the env names them).
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use holon_vcs::engine::symbol_pointer;
use holon_vcs::graph::Graph;
use holon_vcs::model::*;
use holon_vcs::oplog::OpLog;
use holon_vcs::store::{BlobStore, CasMismatch, PointerStore, Revision};
use holon_vcs::{Engine, VcsError};
use tokio::sync::{Barrier, Notify};

/// One set of backends, shareable by several engines (several agents' processes).
pub struct Stores<B, P, G, L> {
    pub blobs: Arc<B>,
    pub pointers: Arc<P>,
    pub graph: Arc<G>,
    pub log: Arc<L>,
}

impl<B, P, G, L> Clone for Stores<B, P, G, L> {
    fn clone(&self) -> Self {
        Stores {
            blobs: self.blobs.clone(),
            pointers: self.pointers.clone(),
            graph: self.graph.clone(),
            log: self.log.clone(),
        }
    }
}

pub type Eng<B, P, G, L> = Engine<Arc<B>, Arc<P>, Arc<G>, Arc<L>>;

impl<B, P, G, L> Stores<B, P, G, L>
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    pub fn engine(&self) -> Arc<Eng<B, P, G, L>> {
        Arc::new(Engine::new(
            self.blobs.clone(),
            self.pointers.clone(),
            self.graph.clone(),
            self.log.clone(),
        ))
    }

    /// An engine whose pointer store is `p` (a racing wrapper) over the same rest.
    pub fn engine_with<Q: PointerStore>(&self, p: Q) -> Engine<Arc<B>, Q, Arc<G>, Arc<L>> {
        Engine::new(self.blobs.clone(), p, self.graph.clone(), self.log.clone())
    }
}

pub const COMPONENT: &str = "orders";
pub const FILE: &str = "src/lib.rs";

pub fn func(name: &str) -> SymbolId {
    SymbolId::new(COMPONENT, FILE, name, SymbolKind::Function)
}

pub fn req(
    ws: &str,
    symbol: SymbolId,
    parent: Option<Hash>,
    change: Transformation,
    agent: &str,
) -> PatchRequest {
    PatchRequest {
        workspace: ws.to_string(),
        symbol,
        parent,
        change,
        agent: Agent::named(agent),
        message: None,
        depends_on: vec![],
        implements: vec![],
        wit_binding: None,
    }
}

pub fn inline(s: &str) -> Content {
    Content::Inline(s.to_string())
}

type E<B, P, G, L> = Engine<B, P, G, L>;

pub async fn create<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &E<B, P, G, L>,
    ws: &str,
    sym: &SymbolId,
    body: &str,
) -> Hash {
    let r = e
        .apply_patch(req(ws, sym.clone(), None, Transformation::Create(inline(body)), "setup"))
        .await
        .unwrap();
    assert_eq!(r.outcome, PatchOutcome::Applied, "{r:?}");
    r.patch
}

pub async fn replace<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &E<B, P, G, L>,
    ws: &str,
    sym: &SymbolId,
    parent: &Hash,
    body: &str,
    agent: &str,
) -> Result<CommitResult, VcsError> {
    e.apply_patch(req(
        ws,
        sym.clone(),
        Some(parent.clone()),
        Transformation::Replace(inline(body)),
        agent,
    ))
    .await
}

pub async fn tip_of<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &E<B, P, G, L>,
    ws: &str,
    sym: &SymbolId,
) -> Option<Hash> {
    match e.query_symbol(ws, SymbolQuery::Symbol(sym.clone())).await {
        Ok(v) => Some(v[0].tip.clone()),
        Err(VcsError::SymbolNotFound(_)) => None,
        Err(e) => panic!("{e}"),
    }
}

pub async fn content_of<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &E<B, P, G, L>,
    ws: &str,
    sym: &SymbolId,
) -> String {
    let v = e.query_symbol(ws, SymbolQuery::Symbol(sym.clone())).await.unwrap();
    match &v[0].content {
        Content::Inline(s) => s.clone(),
        Content::Blob(h) => panic!("expected inline, got blob {h}"),
    }
}

pub async fn file_text<B: BlobStore, P: PointerStore, G: Graph, L: OpLog>(
    e: &E<B, P, G, L>,
    snap: &Snapshot,
    path: &str,
) -> String {
    let entry = snap.entries.iter().find(|t| t.path == path).unwrap_or_else(|| panic!("no {path}"));
    String::from_utf8(e.blobs().get(&entry.blob).await.unwrap().unwrap()).unwrap()
}

// ---- Scenario A --------------------------------------------------------------

/// Two agents, two functions in one file, both from the same state, writing at
/// the same moment: both land, nothing conflicts, the later one reports it
/// commuted past the earlier, and the snapshot has both.
pub async fn scenario_a<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let setup = s.engine();
    let total = func("compute_total");
    let validate = func("validate_order");
    create(&setup, ws, &total, "fn compute_total() -> u32 { 0 }\n").await;
    create(&setup, ws, &validate, "fn validate_order() -> bool { true }\n").await;

    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for (agent, sym, body) in [
        ("agent-1", total.clone(), "fn compute_total() -> u32 { 42 }\n"),
        ("agent-2", validate.clone(), "fn validate_order() -> bool { false }\n"),
    ] {
        let e = s.engine(); // each agent its own engine over the shared stores
        let barrier = barrier.clone();
        let ws = ws.to_string();
        tasks.push(tokio::spawn(async move {
            let parent = tip_of(&e, &ws, &sym).await.unwrap();
            barrier.wait().await; // both have read; now both write
            replace(&e, &ws, &sym, &parent, body, agent).await.unwrap()
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    for r in &results {
        assert!(matches!(r.outcome, PatchOutcome::Applied | PatchOutcome::Commuted), "{r:?}");
        assert_eq!(r.conflict, None);
    }
    results.sort_by_key(|r| r.op);
    let (early, late) = (&results[0], &results[1]);
    assert_eq!(late.outcome, PatchOutcome::Commuted, "{late:?}");
    assert!(late.commuted_with.contains(&early.patch), "{late:?} should list {}", early.patch);
    assert!(!early.commuted_with.contains(&late.patch));

    assert!(setup.list_conflicts(ws, None).await.unwrap().is_empty());
    let snap = setup.snapshot_export(ws, COMPONENT).await.unwrap();
    let text = file_text(&setup, &snap, FILE).await;
    assert_eq!(
        text, "fn compute_total() -> u32 { 42 }\nfn validate_order() -> bool { false }\n",
        "creation order, both edits"
    );
    assert_eq!(snap.at, late.op);
}

// ---- Scenario B --------------------------------------------------------------

/// Two agents, one function, one parent: one lands, the other is a conflict
/// holding both sides verbatim; export is refused until a resolution lands.
pub async fn scenario_b<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let setup = s.engine();
    let total = func("compute_total");
    let base = create(&setup, ws, &total, "fn compute_total() -> u32 { 0 }\n").await;

    let bodies = ["fn compute_total() -> u32 { 1 }\n", "fn compute_total() -> u32 { 2 }\n"];
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let e = s.engine();
        let (barrier, ws, sym, base) =
            (barrier.clone(), ws.to_string(), total.clone(), base.clone());
        let body = body.to_string();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            (i, replace(&e, &ws, &sym, &base, &body, &format!("agent-{i}")).await.unwrap())
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    let applied: Vec<_> =
        results.iter().filter(|(_, r)| r.outcome == PatchOutcome::Applied).collect();
    let conflicted: Vec<_> =
        results.iter().filter(|(_, r)| r.outcome == PatchOutcome::Conflicted).collect();
    assert_eq!((applied.len(), conflicted.len()), (1, 1), "{results:?}");
    let (wi, winner) = applied[0];
    let (li, loser) = conflicted[0];
    let cid = loser.conflict.clone().unwrap();

    // The tip is the winner's, untouched by the conflict.
    assert_eq!(tip_of(&setup, ws, &total).await, Some(winner.patch.clone()));
    assert_eq!(loser.tip, Some(winner.patch.clone()));
    assert_eq!(content_of(&setup, ws, &total).await, bodies[*wi]);

    // Both sides kept verbatim.
    let open = setup.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
    assert_eq!(open.len(), 1);
    let c = &open[0];
    assert_eq!(c.id, cid);
    assert_eq!(c.base, Some(base.clone()));
    assert_eq!(c.left.patch, winner.patch);
    assert_eq!(c.right.patch, loser.patch);
    assert_eq!(c.left.content, Some(inline(bodies[*wi])));
    assert_eq!(c.right.content, Some(inline(bodies[*li])));
    assert_eq!(c.right.agent.id, format!("agent-{li}"));
    assert_eq!(c.opened_at, loser.op);
    let view = setup.query_symbol(ws, SymbolQuery::Symbol(total.clone())).await.unwrap();
    assert_eq!(view[0].open_conflicts, vec![cid.clone()]);

    // Export refused; a forward edit refused; resolution settles it.
    match setup.snapshot_export(ws, COMPONENT).await {
        Err(VcsError::UnresolvedConflict(ids)) => assert_eq!(ids, vec![cid.clone()]),
        other => panic!("{other:?}"),
    }
    match replace(&setup, ws, &total, &winner.patch, "fn compute_total() -> u32 { 9 }\n", "x").await
    {
        Err(VcsError::UnresolvedConflict(ids)) => assert_eq!(ids, vec![cid.clone()]),
        other => panic!("{other:?}"),
    }
    let merged = "fn compute_total() -> u32 { 1 + 2 }\n";
    let res = setup
        .resolve_conflict(ResolutionRequest {
            workspace: ws.to_string(),
            conflict: cid.clone(),
            resolution: Transformation::Replace(inline(merged)),
            agent: Agent::named("resolver"),
            message: Some("both".into()),
        })
        .await
        .unwrap();
    assert_eq!(res.outcome, PatchOutcome::Applied);
    assert_eq!(tip_of(&setup, ws, &total).await, Some(res.patch.clone()));
    let rec = setup.graph().patch(ws, &res.patch).await.unwrap().unwrap();
    let mut parents = vec![winner.patch.clone(), loser.patch.clone()];
    parents.sort();
    let mut got = rec.parents.clone();
    got.sort();
    assert_eq!(got, parents);
    let all = setup.list_conflicts(ws, None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].state, ConflictState::Resolved);
    assert_eq!(all[0].resolved_by, Some(res.patch.clone()));
    let snap = setup.snapshot_export(ws, COMPONENT).await.unwrap();
    assert_eq!(file_text(&setup, &snap, FILE).await, merged);

    // Resolving again with the same content is a duplicate; with other content, refused.
    let again = setup
        .resolve_conflict(ResolutionRequest {
            workspace: ws.to_string(),
            conflict: cid.clone(),
            resolution: Transformation::Replace(inline(merged)),
            agent: Agent::named("resolver-2"),
            message: None,
        })
        .await
        .unwrap();
    assert_eq!(again.outcome, PatchOutcome::Duplicate);
    assert!(matches!(
        setup
            .resolve_conflict(ResolutionRequest {
                workspace: ws.to_string(),
                conflict: cid,
                resolution: Transformation::Replace(inline("other")),
                agent: Agent::named("resolver-3"),
                message: None,
            })
            .await,
        Err(VcsError::Invalid(_))
    ));
}

/// N agents, one function, one parent: exactly one applied, N-1 conflicts (each
/// loser paired with the winner), every patch recorded; resolving them one by one
/// re-points the rest at the new tip until none is left.
pub async fn scenario_b_n_way<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str, n: usize)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let setup = s.engine();
    let total = func("compute_total");
    let base = create(&setup, ws, &total, "fn compute_total() -> u32 { 0 }\n").await;
    let barrier = Arc::new(Barrier::new(n));
    let mut tasks = Vec::new();
    for i in 0..n {
        let e = s.engine();
        let (barrier, ws, sym, base) =
            (barrier.clone(), ws.to_string(), total.clone(), base.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let body = format!("fn compute_total() -> u32 {{ {i} }}\n");
            replace(&e, &ws, &sym, &base, &body, &format!("agent-{i}")).await.unwrap()
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    let winners: Vec<_> = results.iter().filter(|r| r.outcome == PatchOutcome::Applied).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let winner = winners[0].patch.clone();
    let losers: Vec<_> = results.iter().filter(|r| r.outcome == PatchOutcome::Conflicted).collect();
    assert_eq!(losers.len(), n - 1, "{results:?}");
    let ids: BTreeSet<_> = losers.iter().map(|r| r.conflict.clone().unwrap()).collect();
    assert_eq!(ids.len(), n - 1, "one conflict per loser");

    // Zero lost patches: every one is in the graph, and every loser is a side.
    let patches: BTreeSet<_> = results.iter().map(|r| r.patch.clone()).collect();
    assert_eq!(patches.len(), n);
    for p in &patches {
        assert!(setup.graph().patch(ws, p).await.unwrap().is_some(), "patch {p} lost");
    }
    let open = setup.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
    assert_eq!(open.len(), n - 1);
    let rights: BTreeSet<_> = open.iter().map(|c| c.right.patch.clone()).collect();
    let loser_patches: BTreeSet<_> = losers.iter().map(|r| r.patch.clone()).collect();
    assert_eq!(rights, loser_patches);
    assert!(open.iter().all(|c| c.left.patch == winner && c.base == Some(base.clone())));
    assert_eq!(tip_of(&setup, ws, &total).await, Some(winner.clone()));
    // Exactly one op moved the tip; n-1 opened conflicts.
    let ops = setup.oplog(ws, None, 1000).await.unwrap();
    assert_eq!(ops.len(), 1 + n);

    // Resolve one at a time; the rest follow the tip.
    let mut resolved = 0;
    loop {
        let open = setup.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
        let Some(c) = open.first() else { break };
        let tip = tip_of(&setup, ws, &total).await.unwrap();
        assert!(open.iter().all(|o| o.left.patch == tip), "every open conflict is against the tip");
        let res = setup
            .resolve_conflict(ResolutionRequest {
                workspace: ws.to_string(),
                conflict: c.id.clone(),
                resolution: Transformation::Replace(inline(&format!(
                    "fn compute_total() -> u32 {{ r{resolved} }}\n"
                ))),
                agent: Agent::named("resolver"),
                message: None,
            })
            .await
            .unwrap();
        assert_eq!(res.outcome, PatchOutcome::Applied);
        resolved += 1;
        let still = setup.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
        assert_eq!(still.len(), n - 1 - resolved);
    }
    assert_eq!(resolved, n - 1);
    let all = setup.list_conflicts(ws, Some(ConflictState::Resolved)).await.unwrap();
    assert_eq!(all.len(), n - 1);
    // Every loser's patch ended up a parent of some resolution.
    let mut parents = BTreeSet::new();
    for c in &all {
        let r = setup.graph().patch(ws, c.resolved_by.as_ref().unwrap()).await.unwrap().unwrap();
        parents.extend(r.parents);
    }
    assert!(loser_patches.is_subset(&parents));
    setup.snapshot_export(ws, COMPONENT).await.unwrap();
}

// ---- duplicate ---------------------------------------------------------------

pub async fn duplicate<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let e = s.engine();
    let total = func("compute_total");
    let base = create(&e, ws, &total, "fn compute_total() {}\n").await;
    // Creating it again, identically, is a duplicate.
    let again = e
        .apply_patch(req(
            ws,
            total.clone(),
            None,
            Transformation::Create(inline("fn compute_total() {}\n")),
            "b",
        ))
        .await
        .unwrap();
    assert_eq!((again.outcome, again.patch.clone()), (PatchOutcome::Duplicate, base.clone()));

    let a = replace(&e, ws, &total, &base, "v1", "agent-a").await.unwrap();
    // The same change by another agent, and by blob reference rather than inline.
    let blob = e.blobs().put(b"v1".to_vec()).await.unwrap();
    let b = e
        .apply_patch(req(
            ws,
            total.clone(),
            Some(base.clone()),
            Transformation::Replace(Content::Blob(blob)),
            "agent-b",
        ))
        .await
        .unwrap();
    assert_eq!(b.outcome, PatchOutcome::Duplicate);
    assert_eq!(b.patch, a.patch);
    assert_eq!(b.op, a.op);
    // Built on since: a retry of `a` is still a duplicate, not a conflict.
    let c = replace(&e, ws, &total, &a.patch, "v2", "agent-a").await.unwrap();
    let retry = replace(&e, ws, &total, &base, "v1", "agent-a").await.unwrap();
    assert_eq!(retry.outcome, PatchOutcome::Duplicate);
    assert_eq!(retry.tip, Some(c.patch.clone()));
    // Only three ops moved anything; duplicates wrote nothing.
    assert_eq!(e.oplog(ws, None, 100).await.unwrap().len(), 3);

    // Two agents submitting the identical edit at the same moment: one lands.
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for i in 0..2 {
        let e = s.engine();
        let (barrier, ws, sym, parent) =
            (barrier.clone(), ws.to_string(), total.clone(), c.patch.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            replace(&e, &ws, &sym, &parent, "v3", &format!("twin-{i}")).await.unwrap()
        }));
    }
    let mut outs = Vec::new();
    for t in tasks {
        outs.push(t.await.unwrap().outcome);
    }
    outs.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(outs, vec![PatchOutcome::Applied, PatchOutcome::Duplicate]);
    assert!(e.list_conflicts(ws, None).await.unwrap().is_empty());

    // A retried conflicting edit gets the same conflict back, not a second one.
    let tip = tip_of(&e, ws, &total).await.unwrap();
    let late = replace(&e, ws, &total, &c.patch, "v-late", "late").await.unwrap();
    assert_eq!(late.outcome, PatchOutcome::Conflicted);
    let late2 = replace(&e, ws, &total, &c.patch, "v-late", "late").await.unwrap();
    assert_eq!(
        (late2.outcome, late2.conflict.clone()),
        (PatchOutcome::Conflicted, late.conflict.clone())
    );
    assert_eq!(e.list_conflicts(ws, None).await.unwrap().len(), 1);
    assert_eq!(tip_of(&e, ws, &total).await, Some(tip));
}

// ---- revert ------------------------------------------------------------------

pub async fn revert<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let e = s.engine();
    let who = Agent::named("undo");
    let total = func("compute_total");
    let base = create(&e, ws, &total, "v0").await;
    let r1 = replace(&e, ws, &total, &base, "v1", "a").await.unwrap();
    let r2 = replace(&e, ws, &total, &r1.patch, "v2", "a").await.unwrap();

    // Blocked: r1's pointer moved again (by r2). Nothing changes.
    match e.revert_op(ws, r1.op, who.clone()).await {
        Err(VcsError::ConcurrentModification(f)) => {
            assert_eq!(f.expected, Some(r1.patch.clone()));
            assert_eq!(f.actual, Some(r2.patch.clone()));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(content_of(&e, ws, &total).await, "v2");
    assert_eq!(e.oplog(ws, None, 100).await.unwrap().len(), 3);

    // Newest first works, and is itself an op.
    let u2 = e.revert_op(ws, r2.op, who.clone()).await.unwrap();
    assert_eq!(u2.kind, OpKind::Revert(r2.op));
    assert_eq!(u2.moves[0].before, Some(r2.patch.clone()));
    assert_eq!(u2.moves[0].after, Some(r1.patch.clone()));
    assert_eq!(content_of(&e, ws, &total).await, "v1");
    // r1 is still blocked — the revert moved its pointer.
    assert!(matches!(
        e.revert_op(ws, r1.op, who.clone()).await,
        Err(VcsError::ConcurrentModification(_))
    ));
    // Reverting the revert re-applies r2.
    let redo = e.revert_op(ws, u2.id, who.clone()).await.unwrap();
    assert_eq!(redo.kind, OpKind::Revert(u2.id));
    assert_eq!(content_of(&e, ws, &total).await, "v2");
    assert_eq!(tip_of(&e, ws, &total).await, Some(r2.patch.clone()));

    // Reverting the op that only opened a conflict abandons it.
    let stale = replace(&e, ws, &total, &r1.patch, "v-stale", "b").await.unwrap();
    assert_eq!(stale.outcome, PatchOutcome::Conflicted);
    let cid = stale.conflict.clone().unwrap();
    e.revert_op(ws, stale.op, who.clone()).await.unwrap();
    let c = e.list_conflicts(ws, None).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!((c[0].id.clone(), c[0].state), (cid.clone(), ConflictState::Abandoned));
    assert_eq!(tip_of(&e, ws, &total).await, Some(r2.patch.clone()));
    e.snapshot_export(ws, COMPONENT).await.unwrap();

    // Reverting a resolve re-opens its conflict and puts the tip back on `left`.
    let stale = replace(&e, ws, &total, &r1.patch, "v-stale-2", "b").await.unwrap();
    let cid = stale.conflict.clone().unwrap();
    let res = e
        .resolve_conflict(ResolutionRequest {
            workspace: ws.to_string(),
            conflict: cid.clone(),
            resolution: Transformation::Replace(inline("merged")),
            agent: Agent::named("r"),
            message: None,
        })
        .await
        .unwrap();
    assert_eq!(content_of(&e, ws, &total).await, "merged");
    e.revert_op(ws, res.op, who.clone()).await.unwrap();
    assert_eq!(tip_of(&e, ws, &total).await, Some(r2.patch.clone()));
    let open = e.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
    assert_eq!(open.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![cid.clone()]);
    assert_eq!(open[0].resolved_by, None);
    assert!(matches!(e.snapshot_export(ws, COMPONENT).await, Err(VcsError::UnresolvedConflict(_))));
    // …and resolving it again works.
    e.resolve_conflict(ResolutionRequest {
        workspace: ws.to_string(),
        conflict: cid,
        resolution: Transformation::Replace(inline("merged-again")),
        agent: Agent::named("r"),
        message: None,
    })
    .await
    .unwrap();
    assert_eq!(content_of(&e, ws, &total).await, "merged-again");

    // Reverting a create: the symbol is gone, and can be created again.
    let helper = func("helper");
    let h = e
        .apply_patch(req(
            ws,
            helper.clone(),
            None,
            Transformation::Create(inline("fn helper() {}")),
            "a",
        ))
        .await
        .unwrap();
    e.revert_op(ws, h.op, who.clone()).await.unwrap();
    assert_eq!(tip_of(&e, ws, &helper).await, None);
    let h2 = e
        .apply_patch(req(
            ws,
            helper.clone(),
            None,
            Transformation::Create(inline("fn helper() {}")),
            "a",
        ))
        .await
        .unwrap();
    assert_eq!(h2.outcome, PatchOutcome::Applied);
    assert_eq!(content_of(&e, ws, &helper).await, "fn helper() {}");

    assert!(matches!(e.revert_op(ws, 9_999, who).await, Err(VcsError::NotFound(_))));
}

// ---- oplog paging ------------------------------------------------------------

pub async fn oplog_paging<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let e = s.engine();
    let total = func("compute_total");
    let mut parent = create(&e, ws, &total, "v0").await;
    for i in 1..10 {
        parent = replace(&e, ws, &total, &parent, &format!("v{i}"), "a").await.unwrap().patch;
    }
    let all = e.oplog(ws, None, 100).await.unwrap();
    assert_eq!(all.iter().map(|o| o.id).collect::<Vec<_>>(), (1..=10).collect::<Vec<_>>());
    let mut paged = Vec::new();
    let mut after = None;
    loop {
        let page = e.oplog(ws, after, 3).await.unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 3);
        after = Some(page.last().unwrap().id);
        paged.extend(page);
    }
    assert_eq!(paged, all);
    assert!(e.oplog(ws, Some(10), 5).await.unwrap().is_empty());
    assert_eq!(
        e.oplog(ws, Some(4), 2).await.unwrap().iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert!(e.oplog(ws, None, 0).await.unwrap().is_empty());
    // Every entry carries what it moved.
    for w in all.windows(2) {
        assert_eq!(w[1].moves[0].before, w[0].moves[0].after);
    }
    // Other workspaces are other logs.
    assert!(e.oplog(&format!("{ws}/other"), None, 10).await.unwrap().is_empty());

    // Concurrent appends: dense, unique ids.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let e = s.engine();
        let ws = ws.to_string();
        tasks.push(tokio::spawn(async move {
            let sym = func(&format!("f{i}"));
            e.apply_patch(req(&ws, sym, None, Transformation::Create(inline("x")), "a"))
                .await
                .unwrap()
                .op
        }));
    }
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap());
    }
    ids.sort();
    assert_eq!(ids, (11..=18).collect::<Vec<_>>());
}

// ---- delete / rename / validation ---------------------------------------------

pub async fn delete_rename<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let e = s.engine();
    let a = func("a");
    let b = func("b");
    let pa = create(&e, ws, &a, "fn a() {}\n").await;
    let pb = create(&e, ws, &b, "fn b() {}\n").await;

    // Validation.
    let bad = |r: Result<CommitResult, VcsError>| matches!(r, Err(VcsError::Invalid(_)));
    assert!(bad(e
        .apply_patch(req(ws, func("c"), Some(pa.clone()), Transformation::Create(inline("x")), "v"))
        .await));
    assert!(bad(e
        .apply_patch(req(ws, a.clone(), None, Transformation::Replace(inline("x")), "v"))
        .await));
    assert!(bad(e
        .apply_patch(req(
            ws,
            a.clone(),
            Some("nothex".into()),
            Transformation::Replace(inline("x")),
            "v"
        ))
        .await));
    assert!(bad(e
        .apply_patch(req(
            ws,
            SymbolId::new(COMPONENT, "../x", "a", SymbolKind::Function),
            None,
            Transformation::Create(inline("x")),
            "v"
        ))
        .await));
    assert!(matches!(
        e.apply_patch(req(ws, func("missing"), Some(pa.clone()), Transformation::Delete, "v"))
            .await,
        Err(VcsError::SymbolNotFound(_))
    ));
    assert!(matches!(
        e.apply_patch(req(
            ws,
            a.clone(),
            None,
            Transformation::Create(Content::Blob("0".repeat(64))),
            "v"
        ))
        .await,
        Err(VcsError::NotFound(_))
    ));
    assert!(matches!(
        e.apply_patch(req(
            ws,
            a.clone(),
            Some("0".repeat(64)),
            Transformation::Replace(inline("x")),
            "v"
        ))
        .await,
        Err(VcsError::NotFound(_))
    ));
    // A parent from another symbol is not this symbol's history.
    assert!(bad(replace(&e, ws, &a, &pb, "x", "v").await));

    // Rename: the identity moves; the old name is gone; position is kept.
    let ren = e
        .apply_patch(req(ws, a.clone(), Some(pa.clone()), Transformation::Rename("a2".into()), "v"))
        .await
        .unwrap();
    // It commuted past `b`'s create, which landed after `a`'s.
    assert_eq!(
        (ren.outcome, ren.commuted_with.clone()),
        (PatchOutcome::Commuted, vec![pb.clone()])
    );
    assert_eq!(tip_of(&e, ws, &a).await, None);
    let a2 = func("a2");
    assert_eq!(tip_of(&e, ws, &a2).await, Some(ren.patch.clone()));
    assert_eq!(content_of(&e, ws, &a2).await, "fn a() {}\n");
    // Renaming onto a live name is refused.
    assert!(bad(e
        .apply_patch(req(
            ws,
            a2.clone(),
            Some(ren.patch.clone()),
            Transformation::Rename("b".into()),
            "v"
        ))
        .await));
    // Editing through the new name works.
    let ed = replace(&e, ws, &a2, &ren.patch, "fn a2() {}\n", "v").await.unwrap();
    // The old name can be taken by a new symbol, which goes after.
    let fresh = create(&e, ws, &a, "fn a() { /* new */ }\n").await;
    let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
    assert_eq!(file_text(&e, &snap, FILE).await, "fn a2() {}\nfn b() {}\nfn a() { /* new */ }\n");
    let names: Vec<String> = e
        .query_symbol(ws, SymbolQuery::Component(COMPONENT.into()))
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.id.name)
        .collect();
    assert_eq!(names, vec!["a2", "b", "a"]);

    // Delete: gone from queries and the snapshot; editing it is not-found.
    let del = e
        .apply_patch(req(ws, b.clone(), Some(pb.clone()), Transformation::Delete, "v"))
        .await
        .unwrap();
    // `commuted`: the rename, the edit and the new `a` all landed since `pb`'s op.
    assert_eq!((del.outcome, del.tip.clone()), (PatchOutcome::Commuted, None));
    assert_eq!(del.commuted_with.len(), 3);
    assert_eq!(tip_of(&e, ws, &b).await, None);
    assert!(matches!(
        replace(&e, ws, &b, &del.patch, "x", "v").await,
        Err(VcsError::SymbolNotFound(_))
    ));
    let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
    assert_eq!(file_text(&e, &snap, FILE).await, "fn a2() {}\nfn a() { /* new */ }\n");
    // Recreate after delete: a new patch (parented on the delete), same place.
    let back = e
        .apply_patch(req(ws, b.clone(), None, Transformation::Create(inline("fn b() {}\n")), "v"))
        .await
        .unwrap();
    assert_eq!(back.outcome, PatchOutcome::Applied);
    assert_ne!(back.patch, pb);
    let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
    assert_eq!(file_text(&e, &snap, FILE).await, "fn a2() {}\nfn b() {}\nfn a() { /* new */ }\n");
    let _ = (ed, fresh);

    // Graph edges: depends_on / dependents / implements / wit-binding.
    let iface =
        SymbolId::new(COMPONENT, "wit/orders.wit", "orders/api.total", SymbolKind::WitInterface);
    create(&e, ws, &iface, "interface api { total: func() -> u32; }\n").await;
    let caller = func("caller");
    let mut r = req(
        ws,
        caller.clone(),
        None,
        Transformation::Create(inline("fn caller() { a2(); }\n")),
        "v",
    );
    r.depends_on = vec![a2.clone()];
    r.implements = vec![iface.clone()];
    r.wit_binding = Some("holon:orders/api.total".into());
    let cp = e.apply_patch(r).await.unwrap().patch;
    let v = &e.query_symbol(ws, SymbolQuery::Symbol(a2.clone())).await.unwrap()[0];
    assert_eq!(v.dependents, vec![caller.clone()]);
    let v = &e.query_symbol(ws, SymbolQuery::Symbol(caller.clone())).await.unwrap()[0];
    assert_eq!(v.depends_on, vec![a2.clone()]);
    assert_eq!(v.implements, vec![iface.clone()]);
    assert_eq!(v.wit_binding.as_deref(), Some("holon:orders/api.total"));
    assert_eq!(v.author.id, "v");
    // A new body that no longer uses a2 drops the edge (the binding is kept).
    replace(&e, ws, &caller, &cp, "fn caller() {}\n", "v").await.unwrap();
    let v = &e.query_symbol(ws, SymbolQuery::Symbol(a2.clone())).await.unwrap()[0];
    assert!(v.dependents.is_empty());
    let v = &e.query_symbol(ws, SymbolQuery::Symbol(caller)).await.unwrap()[0];
    assert_eq!(v.wit_binding.as_deref(), Some("holon:orders/api.total"));
}

// ---- git tree ids --------------------------------------------------------------

/// The snapshot's `git-tree` is what `git write-tree` says for the same files.
pub async fn git_tree<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let e = s.engine();
    let files: &[(&str, &str, SymbolKind)] = &[
        ("src/lib.rs", "pub fn a() {}", SymbolKind::Function), // no trailing newline
        ("src/lib.rs", "pub fn b() {}\n", SymbolKind::Function),
        ("README.md", "# orders\n", SymbolKind::File),
        ("src/a/b.rs", "mod x;\n", SymbolKind::File),
        // `a-b.rs`, `a.rs` and the directory `a/` — git sorts the directory as
        // `a/`, which lands between `a.rs` ('.' < '/') and nothing after it.
        ("src/a.rs", "mod a;\n", SymbolKind::File),
        ("src/a-b.rs", "// dash\n", SymbolKind::File),
        ("src/a0.rs", "// zero\n", SymbolKind::File),
        ("wit/world.wit", "package a:b;\n", SymbolKind::File),
        ("empty.txt", "", SymbolKind::File),
    ];
    for (i, (path, body, kind)) in files.iter().enumerate() {
        let name = if *kind == SymbolKind::File { path.to_string() } else { format!("item{i}") };
        create(&e, ws, &SymbolId::new(COMPONENT, path, &name, *kind), body).await;
    }
    let snap = e.snapshot_export(ws, COMPONENT).await.unwrap();
    assert_eq!(file_text(&e, &snap, "src/lib.rs").await, "pub fn a() {}\npub fn b() {}\n");

    let dir = std::env::temp_dir().join(format!(
        "holon-vcs-git-{}-{}",
        std::process::id(),
        ws.replace('/', "_")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for t in &snap.entries {
        let p = dir.join(&t.path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, e.blobs().get(&t.blob).await.unwrap().unwrap()).unwrap();
    }
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    let want = git(&["write-tree"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(snap.git_tree.as_deref(), Some(want.as_str()));
    assert_eq!(snap.entries.len(), files.len() - 1);
}

// ---- CAS retry correctness -----------------------------------------------------

/// A pointer store that can stop a CAS just before it happens, let the test do
/// something to the real store, and then let it go — the interleaving a real
/// race produces, on demand.
pub struct Racing<P> {
    pub inner: Arc<P>,
    armed: AtomicBool,
    reached: Notify,
    go: Notify,
    /// Bump (rewrite unchanged) the key before this many further CASes.
    pub bump_next: AtomicU32,
    pub cas_calls: AtomicU32,
}

impl<P> Racing<P> {
    pub fn new(inner: Arc<P>) -> Self {
        Racing {
            inner,
            armed: AtomicBool::new(false),
            reached: Notify::new(),
            go: Notify::new(),
            bump_next: AtomicU32::new(0),
            cas_calls: AtomicU32::new(0),
        }
    }
    /// The next CAS will wait for [`Racing::release`] after signalling.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    pub async fn reached(&self) {
        self.reached.notified().await;
    }
    pub fn release(&self) {
        self.go.notify_one();
    }
}

impl<P: PointerStore> PointerStore for Racing<P> {
    async fn get(&self, key: &str) -> holon_vcs::error::Result<Option<(Option<Hash>, Revision)>> {
        self.inner.get(key).await
    }

    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Option<&str>,
    ) -> holon_vcs::error::Result<Result<Revision, CasMismatch>> {
        self.cas_calls.fetch_add(1, Ordering::SeqCst);
        if self.armed.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.go.notified().await;
        }
        if self.bump_next.load(Ordering::SeqCst) > 0 {
            self.bump_next.fetch_sub(1, Ordering::SeqCst);
            if let Some((v, rev)) = self.inner.get(key).await? {
                self.inner.cas(key, Some(rev), v.as_deref()).await?.expect("bump");
            }
        }
        self.inner.cas(key, expected, value).await
    }
}

pub async fn cas_retry<B, P, G, L>(s: Stores<B, P, G, L>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let other = s.engine();
    let total = func("compute_total");
    let validate = func("validate_order");
    let base = create(&other, ws, &total, "v0").await;
    let vbase = create(&other, ws, &validate, "ok").await;
    let racing = Arc::new(Racing::new(s.pointers.clone()));
    let victim = Arc::new(s.engine_with(racing.clone()));

    // 1. Another agent's edit to the SAME symbol lands between the victim's read
    //    and its CAS: the victim re-reads and becomes a conflict against it.
    racing.arm();
    let t = {
        let (v, ws, sym, base) = (victim.clone(), ws.to_string(), total.clone(), base.clone());
        tokio::spawn(async move { replace(&*v, &ws, &sym, &base, "victim", "victim").await })
    };
    racing.reached().await;
    let won = replace(&other, ws, &total, &base, "other", "other").await.unwrap();
    racing.release();
    let r = t.await.unwrap().unwrap();
    assert_eq!(r.outcome, PatchOutcome::Conflicted, "{r:?}");
    let c = &other.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap()[0];
    assert_eq!((c.left.patch.clone(), c.right.patch.clone()), (won.patch.clone(), r.patch.clone()));
    assert_eq!(tip_of(&other, ws, &total).await, Some(won.patch.clone()));
    other
        .resolve_conflict(ResolutionRequest {
            workspace: ws.to_string(),
            conflict: c.id.clone(),
            resolution: Transformation::Replace(inline("v1")),
            agent: Agent::named("r"),
            message: None,
        })
        .await
        .unwrap();
    let tip = tip_of(&other, ws, &total).await.unwrap();

    // 2. Another agent's edit to a DIFFERENT symbol lands in the same window: the
    //    victim's CAS is on another pointer, so it lands first try — and reports
    //    that it commuted past it.
    racing.arm();
    let before = racing.cas_calls.load(Ordering::SeqCst);
    let t = {
        let (v, ws, sym, tip) = (victim.clone(), ws.to_string(), total.clone(), tip.clone());
        tokio::spawn(async move { replace(&*v, &ws, &sym, &tip, "v2", "victim").await })
    };
    racing.reached().await;
    let side = replace(&other, ws, &validate, &vbase, "still ok", "other").await.unwrap();
    racing.release();
    let r = t.await.unwrap().unwrap();
    assert_eq!(r.outcome, PatchOutcome::Commuted, "{r:?}");
    assert_eq!(r.commuted_with, vec![side.patch.clone()]);
    assert_eq!(racing.cas_calls.load(Ordering::SeqCst) - before, 1, "no retry needed");

    // 3. The pointer is rewritten with the SAME value (a revision bump — what a
    //    conflict's no-op CAS does): the victim loses the CAS, re-reads, and lands
    //    once. Exactly one op for it.
    let ops_before = other.oplog(ws, None, 1000).await.unwrap().len();
    racing.bump_next.store(1, Ordering::SeqCst);
    let before = racing.cas_calls.load(Ordering::SeqCst);
    let r3 = replace(&*victim, ws, &total, &r.patch, "v3", "victim").await.unwrap();
    assert_eq!(r3.outcome, PatchOutcome::Applied);
    assert_eq!(racing.cas_calls.load(Ordering::SeqCst) - before, 2, "one lost CAS, one landed");
    assert_eq!(other.oplog(ws, None, 1000).await.unwrap().len(), ops_before + 1);
    assert_eq!(content_of(&other, ws, &total).await, "v3");

    // 4. Losing every race: bounded, `concurrent-modification`, nothing landed.
    let bounded = s.engine_with(racing.clone()).with_cas_tries(5);
    racing.bump_next.store(5, Ordering::SeqCst);
    let ops_before = other.oplog(ws, None, 1000).await.unwrap().len();
    match replace(&bounded, ws, &total, &r3.patch, "v4", "victim").await {
        Err(VcsError::ConcurrentModification(f)) => {
            let key = f.pointer.rsplit('/').next().unwrap();
            assert_eq!(f.pointer, symbol_pointer(ws, key));
            assert_eq!(f.actual, Some(r3.patch.clone()));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(content_of(&other, ws, &total).await, "v3");
    assert_eq!(other.oplog(ws, None, 1000).await.unwrap().len(), ops_before);

    // 5. A conflict and a forward edit race: the conflict's record is written
    //    before its (no-op) CAS, so an applier that reads in between sees it and
    //    refuses — it cannot build on a tip a conflict is about to name as `left`.
    racing.arm();
    let t = {
        let (v, ws, sym, parent) = (victim.clone(), ws.to_string(), total.clone(), r.patch.clone());
        tokio::spawn(async move { replace(&*v, &ws, &sym, &parent, "stale", "victim").await })
    };
    racing.reached().await;
    let fwd = replace(&other, ws, &total, &r3.patch, "forward", "other").await;
    assert!(matches!(fwd, Err(VcsError::UnresolvedConflict(_))), "{fwd:?}");
    racing.release();
    let r5 = t.await.unwrap().unwrap();
    assert_eq!(r5.outcome, PatchOutcome::Conflicted);
    let open = other.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].left.patch, r3.patch);
    assert_eq!(open[0].opened_at, r5.op);
}

/// One `#[tokio::test]` per scenario, each given fresh `(Stores, workspace)` by
/// `$make(name)`, which may return `None` to skip.
#[macro_export]
macro_rules! scenario_tests {
    ($make:path) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn scenario_a_disjoint_symbols_commute() {
            let Some((s, ws)) = $make("scenario-a").await else { return };
            common::scenario_a(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn scenario_b_same_symbol_conflicts_then_resolves() {
            let Some((s, ws)) = $make("scenario-b").await else { return };
            common::scenario_b(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn scenario_b_eight_agents_race() {
            let Some((s, ws)) = $make("scenario-b8").await else { return };
            common::scenario_b_n_way(s, &ws, 8).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn duplicates() {
            let Some((s, ws)) = $make("duplicate").await else { return };
            common::duplicate(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn revert() {
            let Some((s, ws)) = $make("revert").await else { return };
            common::revert(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn oplog_paging() {
            let Some((s, ws)) = $make("oplog").await else { return };
            common::oplog_paging(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn delete_rename_edges_validation() {
            let Some((s, ws)) = $make("delete-rename").await else { return };
            common::delete_rename(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn git_tree_matches_git() {
            let Some((s, ws)) = $make("git-tree").await else { return };
            common::git_tree(s, &ws).await;
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn cas_retry() {
            let Some((s, ws)) = $make("cas-retry").await else { return };
            common::cas_retry(s, &ws).await;
        }
    };
}
