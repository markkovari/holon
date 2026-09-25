//! The `holon:vcs/code-store` operations, over the four storage traits.
//!
//! # The write path
//!
//! Every mutation is: **read** the pointers it depends on (value, revision, and
//! the op each value names) and the graph state that matters → [`decide`] →
//! **log the intent** (the op, `pending`, with the revision every pointer write
//! expects and every record it introduces) → **write the graph ahead** (patch
//! records, index entries, conflict records, uncommitted) → **claim** names →
//! **compare-and-set the symbol's tip** — the commit point — → **finish**
//! (mirrors, conflict states, name releases) → mark the op `committed`. A lost
//! CAS undoes the claims, abandons the op's uncommitted conflicts and marks it
//! `aborted`; the call re-reads and re-decides, bounded by
//! [`Engine::with_cas_tries`] (default [`CAS_TRIES`]), and a caller that
//! exhausts it gets `concurrent-modification`. Why each crash point in that
//! sequence is recoverable is [`crate::recovery`]'s module doc.
//!
//! **A conflict also CASes the tip — to the value it already has.** That bumps
//! the revision, and it is what makes "is there an open conflict?" safe to ask:
//! the conflict record is written *before* that CAS, and an applier reads the
//! open conflicts *after* reading the tip. Either the conflict's CAS happened
//! before the applier's read (so the applier sees the record and refuses with
//! `unresolved-conflict`), or both CAS against the same revision and exactly one
//! lands (the loser re-decides). An uncommitted conflict (`opened-at: 0`)
//! counts as open for that reason; it is committed when its op commits, and
//! abandoned when the last op that wrote it aborts.
//!
//! # Outcomes, precisely
//!
//! * `applied` — the tip moved from `parent` to the new patch, and nothing
//!   below counts as commuted.
//! * `commuted` — the tip moved from `parent` (so `parent` WAS the tip), AND at
//!   least one other op after the request's `read-at` that had landed when this
//!   one did moved the tip of ANOTHER symbol in the same component;
//!   `commuted-with` lists those patches, in log order. Ops that only opened a
//!   conflict, and reverts, do not count. `read-at` is the position the agent's
//!   view reflects (a view's `as-of`), so this is exactly "what landed that the
//!   agent had not seen". Without `read-at` it is measured from the op that
//!   landed `parent` instead — an over-approximation: it also lists edits the
//!   agent may have read after fetching `parent` — and a `create` never
//!   commutes. "Had landed" is checked just after this patch's commit point,
//!   over the whole log after `read-at` (an op logged after this one may have
//!   landed first), so of two racing edits to two symbols from one view, the
//!   one that lands second always lists the other, and both may list each
//!   other.
//! * `conflicted` — see [`crate::patch`]. The tip does not move. In an N-way race
//!   from one parent, one edit is `applied` and each of the other N-1 opens its
//!   own conflict `(winner, loser)`: every patch is kept.
//! * `duplicate` — nothing was written; `op` is the op that landed the patch.
//!
//! # Names
//!
//! A live symbol id is reserved by a name pointer `ws/<w>/name/<name key>`
//! holding the symbol key that has it. `create` claims it (CAS from free, or
//! from a stale holder — one whose tip is no longer that id — to its key),
//! `rename` claims the new name and releases the old one, `delete` releases it;
//! a revert moves them back. Claims happen before the tip CAS and are undone if
//! it loses; releases after it. So of N creates and renames racing for one name
//! exactly one claim lands, and the others re-read and get
//! [`VcsError::NameTaken`] — except two `create`s of the SAME symbol id, which
//! are two versions of one symbol, not two symbols: the loser becomes a
//! conflict (or a duplicate), as in step two. A create is `name-taken` when the
//! holder got the name by a rename (its key is not one the id would be created
//! at).
//!
//! # Resolution with several open conflicts
//!
//! `resolve-conflict` moves the tip from the conflict's `left` to a resolution
//! whose parents are `[left, right]`. Any OTHER open conflict on that symbol was
//! `(left, other)`; its left is no longer the tip, so in the same op it is
//! abandoned and re-opened as `(resolution, other)` with the same base.
//!
//! # Revert
//!
//! `revert-op` refuses (`concurrent-modification`, nothing changed) if any
//! pointer the op moved was touched by a LATER op — including a conflict's no-op
//! move, since that conflict names the value as its `left` — or does not hold the
//! op's `after` value now. Otherwise it logs `revert(op)` with the inverse moves
//! and the inverse of the op's conflict effects, and carries it out like any
//! other op (so reverting an apply that only opened a conflict abandons it;
//! reverting a resolve re-opens the conflict and undoes the sibling
//! re-pointing; reverting a revert re-applies). An op that is still pending is
//! settled first; an aborted one is `not-found`.
//!
//! # Positions
//!
//! A file is its live symbols sorted by order key, ties broken by symbol key
//! ([`crate::order`]). `patch-request.position` places a `create`
//! (`first`/`last`/`after(s)`/`before(s)`); the transformation `move(placement)`
//! re-places an existing symbol (a patch like any other: two concurrent moves
//! of one symbol conflict). Unplaced creates keep step two's creation order.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::{Result, VcsError};
use crate::git;
use crate::graph::{
    Change, ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SideRecord,
};
use crate::model::{
    Agent, CasFailure, CommitResult, Conflict, ConflictId, ConflictSide, ConflictState, Content,
    Hash, OpEntry, OpId, OpKind, PatchOutcome, PatchRequest, Placement, PointerMove,
    ResolutionRequest, Snapshot, SymbolId, SymbolQuery, SymbolView, Transformation, TreeEntry,
};
use crate::oplog::{Guard, Intent, NewOp, OpLog, OpState};
use crate::order;
use crate::patch::{
    self, conflict_id, decide, is_probe_key, name_key, patch_hash, symbol_key, Decision, Inputs,
};
use crate::store::{escape, is_hash, BlobStore, PointerStore, PointerValue, Revision};

/// How many CAS races one operation may lose before `concurrent-modification`.
/// Every loss is somebody else's landed write, so this bounds starvation.
pub const CAS_TRIES: u32 = 200;
/// Probe indexes tried when a name's natural key is held by a symbol since renamed.
pub const MAX_PROBE: u32 = 64;
/// Content at most this long, and UTF-8, is returned `inline`; larger as `blob`.
pub const INLINE_MAX: usize = 64 * 1024;
/// A snapshot re-reads while the oplog moves under it, this many times.
pub const SNAPSHOT_TRIES: u32 = 64;
/// How long a pending op is presumed in flight before a reader or repair may
/// fence it off and abort it. Fencing a live writer is safe — its CAS loses and
/// it retries — so this is a liveness knob, not a correctness one.
pub const LEASE_MS: u64 = 30_000;

/// The pointer a symbol's tip lives at: `ws/<escaped workspace>/sym/<key>`.
pub fn symbol_pointer(ws: &str, key: &str) -> String {
    format!("ws/{}/sym/{key}", escape(ws))
}

/// The pointer reserving a symbol id: `ws/<escaped workspace>/name/<name key>`.
pub fn name_pointer(ws: &str, id: &SymbolId) -> String {
    format!("ws/{}/name/{}", escape(ws), name_key(id))
}

pub(crate) fn key_of_pointer(pointer: &str) -> Option<&str> {
    pointer.rsplit_once("/sym/").map(|(_, k)| k)
}

pub(crate) fn is_name_pointer(pointer: &str) -> bool {
    pointer.contains("/name/")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Yield to the executor once — any executor: no runtime is assumed.
pub(crate) fn yield_now() -> impl Future<Output = ()> {
    struct YieldNow(bool);
    impl Future for YieldNow {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    YieldNow(false)
}

/// A symbol pointer as read, with the patch its value names.
#[derive(Debug, Clone)]
pub(crate) struct TipRead {
    pub pointer: String,
    pub rev: Option<Revision>,
    pub tip: Option<PatchRecord>,
    pub op: Option<OpId>,
}

impl TipRead {
    fn live_as(&self, id: &SymbolId) -> bool {
        self.tip.as_ref().is_some_and(|t| !t.is_delete() && t.symbol == *id)
    }
}

/// A name pointer as read.
#[derive(Debug, Clone)]
pub(crate) struct NameSlot {
    pub pointer: String,
    pub rev: Option<Revision>,
    pub value: PointerValue,
}

impl NameSlot {
    fn held_by(&self, key: &str) -> bool {
        self.value.value.as_deref() == Some(key)
    }
    fn guard(&self) -> Guard {
        Guard { rev: self.rev, op: self.value.op }
    }
}

pub(crate) enum NameRead {
    /// Held by the live symbol `key`, whose tip is read.
    Held(NameSlot, String, Box<TipRead>),
    /// Free, or held by a symbol that is no longer called this (stale).
    Free(NameSlot),
    /// A claim on it is in flight; read again.
    Busy,
}

/// A symbol located by id: its key, its pointer, the tip there, and the name
/// pointer reserving the id.
struct Located {
    key: String,
    pointer: String,
    rev: Option<Revision>,
    tip: Option<PatchRecord>,
    tip_op: Option<OpId>,
    name: NameSlot,
}

impl Located {
    fn tip_hash(&self) -> Option<Hash> {
        self.tip.as_ref().map(|t| t.hash.clone())
    }
    fn live_tip(&self) -> Option<Hash> {
        self.tip.as_ref().filter(|t| !t.is_delete()).map(|t| t.hash.clone())
    }
    fn guard(&self) -> Guard {
        Guard { rev: self.rev, op: self.tip_op }
    }
}

pub struct Engine<B, P, G, L> {
    pub(crate) blobs: B,
    pub(crate) pointers: P,
    pub(crate) graph: G,
    pub(crate) log: L,
    pub(crate) clock: fn() -> u64,
    pub(crate) cas_tries: u32,
    pub(crate) lease_ms: u64,
}

impl<B, P, G, L> Engine<B, P, G, L>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    pub fn new(blobs: B, pointers: P, graph: G, log: L) -> Self {
        Engine {
            blobs,
            pointers,
            graph,
            log,
            clock: now_ms,
            cas_tries: CAS_TRIES,
            lease_ms: LEASE_MS,
        }
    }

    pub fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_cas_tries(mut self, n: u32) -> Self {
        self.cas_tries = n.max(1);
        self
    }

    /// How old a pending op must be before it is presumed dead ([`LEASE_MS`]).
    pub fn with_lease_ms(mut self, ms: u64) -> Self {
        self.lease_ms = ms;
        self
    }

    pub fn blobs(&self) -> &B {
        &self.blobs
    }
    pub fn pointers(&self) -> &P {
        &self.pointers
    }
    pub fn graph(&self) -> &G {
        &self.graph
    }
    pub fn log(&self) -> &L {
        &self.log
    }

    // ---- reading pointers ------------------------------------------------------

    /// Read a symbol pointer and the patch it names. If the op that last wrote
    /// the pointer may not be finished — the patch record is missing or still
    /// `pending`, or the pointer was last written by some other op than the one
    /// that landed the patch (a conflict's no-op write, a revert) — that op is
    /// settled first, so every decision is made on finished state.
    pub(crate) async fn read_tip(&self, ws: &str, key: &str) -> Result<TipRead> {
        let pointer = symbol_pointer(ws, key);
        let Some((v, rev)) = self.pointers.get(&pointer).await? else {
            return Ok(TipRead { pointer, rev: None, tip: None, op: None });
        };
        let Some(h) = v.value.clone() else {
            if let Some(op) = v.op {
                self.settle(ws, op).await?;
            }
            return Ok(TipRead { pointer, rev: Some(rev), tip: None, op: v.op });
        };
        let mut rec = self.graph.patch(ws, &h).await?;
        let unsure = rec.as_ref().is_none_or(|r| r.status == PatchStatus::Pending || r.op != v.op);
        if let (true, Some(op)) = (unsure, v.op) {
            if self.settle(ws, op).await? != OpState::Pending {
                rec = self.graph.patch(ws, &h).await?;
            }
        }
        let rec = rec.ok_or_else(|| {
            VcsError::Storage(format!(
                "pointer {pointer} names patch {h}, which the graph does not have"
            ))
        })?;
        Ok(TipRead { pointer, rev: Some(rev), tip: Some(rec), op: v.op })
    }

    /// Read the name pointer reserving `id`.
    pub(crate) async fn read_name(&self, ws: &str, id: &SymbolId) -> Result<NameRead> {
        let pointer = name_pointer(ws, id);
        let (value, rev) = match self.pointers.get(&pointer).await? {
            Some((v, r)) => (v, Some(r)),
            None => (PointerValue::default(), None),
        };
        let slot = NameSlot { pointer, rev, value };
        let Some(holder) = slot.value.value.clone() else { return Ok(NameRead::Free(slot)) };
        let t = self.read_tip(ws, &holder).await?;
        if t.live_as(id) {
            return Ok(NameRead::Held(slot, holder, Box::new(t)));
        }
        // Held for a symbol that is not called this — as of the tip we read.
        // Either a claim is in flight (its op is pending), or was aborted and
        // not yet undone, or it committed after we read the tip (so read it
        // again), or the holder has since moved away and its release has not
        // landed: only that last is stale, and free.
        if let Some(j) = slot.value.op {
            match self.settle(ws, j).await? {
                OpState::Pending => return Ok(NameRead::Busy),
                OpState::Aborted => {
                    if let Some(op) = self.log.get(ws, j).await? {
                        self.roll_back(ws, &op).await?;
                    }
                    return Ok(NameRead::Busy);
                }
                OpState::Committed => {
                    let t = self.read_tip(ws, &holder).await?;
                    if t.live_as(id) {
                        return Ok(NameRead::Held(slot, holder, Box::new(t)));
                    }
                }
            }
        }
        Ok(NameRead::Free(slot))
    }

    /// Find where `id` lives, authoritatively: its name pointer if it is live,
    /// else a dead symbol that was last called `id`, else the first free probe
    /// slot — where a create would go.
    async fn locate(&self, ws: &str, id: &SymbolId) -> Result<Located> {
        for _ in 0..self.cas_tries {
            if let Some(l) = self.try_locate(ws, id).await? {
                return Ok(l);
            }
            yield_now().await;
        }
        Err(VcsError::ConcurrentModification(CasFailure {
            pointer: name_pointer(ws, id),
            expected: None,
            actual: None,
        }))
    }

    async fn try_locate(&self, ws: &str, id: &SymbolId) -> Result<Option<Located>> {
        let located = |key: String, t: TipRead, name: NameSlot| Located {
            key,
            pointer: t.pointer,
            rev: t.rev,
            tip: t.tip,
            tip_op: t.op,
            name,
        };
        let name = match self.read_name(ws, id).await? {
            NameRead::Busy => return Ok(None),
            NameRead::Held(slot, key, t) => return Ok(Some(located(key, *t, slot))),
            NameRead::Free(slot) => slot,
        };
        for key in self.graph.symbols_named(ws, id).await? {
            let t = self.read_tip(ws, &key).await?;
            if t.tip.as_ref().is_some_and(|r| r.symbol == *id) {
                return Ok(Some(located(key, t, name)));
            }
        }
        for n in 0..MAX_PROBE {
            let key = symbol_key(id, n);
            let t = self.read_tip(ws, &key).await?;
            match &t.tip {
                None => return Ok(Some(located(key, t, name))),
                Some(r) if r.symbol == *id => return Ok(Some(located(key, t, name))),
                Some(_) => {}
            }
        }
        Err(VcsError::Invalid(format!("more than {MAX_PROBE} symbols have been called {id}")))
    }

    // ---- apply-patch ---------------------------------------------------------

    /// Record one edit: it lands, commutes, duplicates, or becomes a conflict.
    pub async fn apply_patch(&self, req: PatchRequest) -> Result<CommitResult> {
        validate_request(&req)?;
        let ws = req.workspace.as_str();
        let change = self.normalise(&req.change).await?;
        let mut last = None;
        for _ in 0..self.cas_tries {
            let loc = self.locate(ws, &req.symbol).await?;
            // AFTER the tip read — see the module notes on why the order matters.
            let open = self.graph.open_conflicts_for(ws, &loc.key).await?;
            let placement = req.position.as_ref();
            let h0 = patch::request_hash(&loc.key, req.parent.as_ref(), &change, placement);
            let recorded = self.graph.patch(ws, &h0).await?;
            let recorded_conflict = match recorded.as_ref().map(|r| &r.status) {
                Some(PatchStatus::Conflicted(c)) => self.graph.conflict(ws, c).await?,
                _ => None,
            };
            let tip_hash = loc.tip_hash();
            let parent_record = match &req.parent {
                Some(p) if Some(p) != tip_hash.as_ref() => self.graph.patch(ws, p).await?,
                _ => None,
            };
            // A retry of an edit that already landed and took the symbol away
            // from this name (a rename): the name no longer finds it, the parent
            // does.
            if let (None, Some(parent)) = (loc.live_tip(), &req.parent) {
                if let Some(prec) = self.graph.patch(ws, parent).await? {
                    let h = patch::request_hash(&prec.key, Some(parent), &change, None);
                    if let Some(done) = self.graph.patch(ws, &h).await? {
                        if done.status == PatchStatus::Landed && done.key != loc.key {
                            return Ok(CommitResult {
                                patch: h,
                                op: done.op.unwrap_or(0),
                                outcome: PatchOutcome::Duplicate,
                                tip: self.live_tip_of(ws, &prec.key).await?,
                                commuted_with: vec![],
                                conflict: None,
                            });
                        }
                    }
                }
            }
            // A create of a name another symbol took by renaming is not a second
            // version of that symbol: it is a different symbol wanting its name.
            if matches!(change, Change::Create(_))
                && loc.live_tip().is_some()
                && !is_probe_key(&req.symbol, &loc.key, MAX_PROBE)
            {
                return Err(VcsError::NameTaken(req.symbol.clone()));
            }
            let decision = match decide(&Inputs {
                key: &loc.key,
                symbol: &req.symbol,
                parent: req.parent.as_ref(),
                change: &change,
                placement,
                tip: loc.tip.as_ref(),
                recorded: recorded.as_ref(),
                parent_record: parent_record.as_ref(),
                open: &open,
                recorded_conflict: recorded_conflict.as_ref(),
            }) {
                Ok(d) => d,
                Err(e) => {
                    // A refusal computed from a tip that has since moved is not an
                    // answer: e.g. an open conflict whose `left` is a tip we read too
                    // early to see. Only refuse on a tip that is still current.
                    if self.moved_since(&loc).await? {
                        last = Some((loc.pointer.clone(), tip_hash));
                        continue;
                    }
                    return Err(e);
                }
            };
            let landed = match decision {
                Decision::Duplicate { patch } => {
                    let op = match self.graph.patch(ws, &patch).await?.and_then(|p| p.op) {
                        Some(op) => op,
                        None => self.oplog_head(ws).await?,
                    };
                    return Ok(CommitResult {
                        patch,
                        op,
                        outcome: PatchOutcome::Duplicate,
                        tip: loc.live_tip(),
                        commuted_with: vec![],
                        conflict: None,
                    });
                }
                Decision::AlreadyConflicted { patch, conflict } => {
                    let op =
                        self.graph.conflict(ws, &conflict).await?.map(|c| c.opened_at).unwrap_or(0);
                    return Ok(CommitResult {
                        patch,
                        op,
                        outcome: PatchOutcome::Conflicted,
                        tip: loc.live_tip(),
                        commuted_with: vec![],
                        conflict: Some(conflict),
                    });
                }
                Decision::Applied { patch, parents } => {
                    self.land(&req, &loc, &change, patch, parents).await?
                }
                Decision::Conflicted { patch, parents, base, left } => {
                    self.open_conflict(
                        &req,
                        &loc,
                        &change,
                        parent_record.as_ref(),
                        patch,
                        parents,
                        base,
                        left,
                    )
                    .await?
                }
            };
            match landed {
                Some(r) => return Ok(r),
                None => {
                    last = Some((loc.pointer.clone(), tip_hash));
                    yield_now().await;
                }
            }
        }
        Err(self.exhausted(last).await)
    }

    /// Whether `loc`'s pointer is no longer at the revision it was read at.
    async fn moved_since(&self, loc: &Located) -> Result<bool> {
        let now = self.pointers.get(&loc.pointer).await?.map(|(_, r)| r);
        Ok(now != loc.rev)
    }

    async fn exhausted(&self, last: Option<(String, Option<Hash>)>) -> VcsError {
        let (pointer, expected) = last.unwrap_or_default();
        let actual = match self.pointers.get(&pointer).await {
            Ok(v) => v.and_then(|(v, _)| v.value),
            Err(e) => return e,
        };
        VcsError::ConcurrentModification(CasFailure { pointer, expected, actual })
    }

    /// Inline content goes to the blob store; a blob reference must exist there.
    async fn normalise(&self, t: &Transformation) -> Result<Change> {
        Ok(match t {
            Transformation::Create(c) => Change::Create(self.content_hash(c).await?),
            Transformation::Replace(c) => Change::Replace(self.content_hash(c).await?),
            Transformation::Delete => Change::Delete,
            Transformation::Rename(n) => {
                if n.is_empty() {
                    return Err(VcsError::Invalid("rename to an empty name".into()));
                }
                Change::Rename(n.clone())
            }
            Transformation::Move(p) => Change::Move(p.clone()),
        })
    }

    async fn content_hash(&self, c: &Content) -> Result<Hash> {
        match c {
            Content::Inline(s) => self.blobs.put(s.as_bytes().to_vec()).await,
            Content::Blob(h) => {
                if !is_hash(h) {
                    return Err(VcsError::Invalid(format!(
                        "blob {h:?} is not 64 lower-case hex characters"
                    )));
                }
                if !self.blobs.contains(h).await? {
                    return Err(VcsError::NotFound(format!("blob {h}")));
                }
                Ok(h.clone())
            }
        }
    }

    pub(crate) async fn patch_required(&self, ws: &str, h: &str) -> Result<PatchRecord> {
        self.graph.patch(ws, h).await?.ok_or_else(|| {
            VcsError::Storage(format!("a pointer names patch {h}, which the graph does not have"))
        })
    }

    /// Move the tip. `Ok(None)`: lost a CAS (or a name claim is in flight);
    /// nothing is left behind but an aborted op.
    async fn land(
        &self,
        req: &PatchRequest,
        loc: &Located,
        change: &Change,
        patch: Hash,
        parents: Vec<Hash>,
    ) -> Result<Option<CommitResult>> {
        let ws = req.workspace.as_str();
        let tip = loc.tip.as_ref();
        let carried_order = tip.and_then(|t| t.order.clone());
        let mut claims: Vec<(PointerMove, Guard)> = Vec::new();
        let mut releases: Vec<(PointerMove, Guard)> = Vec::new();
        let release_own = |releases: &mut Vec<(PointerMove, Guard)>| {
            if loc.name.held_by(&loc.key) {
                releases.push((
                    PointerMove {
                        pointer: loc.name.pointer.clone(),
                        before: Some(loc.key.clone()),
                        after: None,
                    },
                    loc.name.guard(),
                ));
            }
        };
        let (symbol, content, depends_on, implements, wit_binding, order) = match change {
            Change::Create(c) | Change::Replace(c) => {
                if matches!(change, Change::Create(_)) {
                    claims.push((
                        PointerMove {
                            pointer: loc.name.pointer.clone(),
                            before: loc.name.value.value.clone(),
                            after: Some(loc.key.clone()),
                        },
                        loc.name.guard(),
                    ));
                }
                let order = match &req.position {
                    Some(p) => Some(self.resolve_order(ws, &req.symbol, &loc.key, p).await?),
                    None => carried_order,
                };
                (
                    req.symbol.clone(),
                    Some(c.clone()),
                    req.depends_on.clone(),
                    req.implements.clone(),
                    req.wit_binding.clone().or_else(|| tip.and_then(|t| t.wit_binding.clone())),
                    order,
                )
            }
            Change::Delete => {
                release_own(&mut releases);
                (req.symbol.clone(), None, vec![], vec![], None, carried_order)
            }
            Change::Rename(name) => {
                let t = tip.expect("decide: rename needs a live tip");
                let renamed = t.symbol.renamed(name);
                if renamed == t.symbol {
                    return Err(VcsError::Invalid(format!(
                        "{} is already called {name}",
                        t.symbol
                    )));
                }
                match self.read_name(ws, &renamed).await? {
                    NameRead::Busy => return Ok(None),
                    NameRead::Held(_, holder, _) if holder != loc.key => {
                        return Err(VcsError::NameTaken(renamed));
                    }
                    NameRead::Held(slot, _, _) | NameRead::Free(slot) => {
                        claims.push((
                            PointerMove {
                                pointer: slot.pointer.clone(),
                                before: slot.value.value.clone(),
                                after: Some(loc.key.clone()),
                            },
                            slot.guard(),
                        ));
                    }
                }
                release_own(&mut releases);
                (
                    renamed,
                    t.content.clone(),
                    t.depends_on.clone(),
                    t.implements.clone(),
                    t.wit_binding.clone(),
                    carried_order,
                )
            }
            Change::Move(p) => {
                let t = tip.expect("decide: move needs a live tip");
                let order = self.resolve_order(ws, &t.symbol, &loc.key, p).await?;
                (
                    t.symbol.clone(),
                    t.content.clone(),
                    t.depends_on.clone(),
                    t.implements.clone(),
                    t.wit_binding.clone(),
                    Some(order),
                )
            }
        };
        let at = (self.clock)();
        let rec = PatchRecord {
            hash: patch.clone(),
            key: loc.key.clone(),
            symbol: symbol.clone(),
            parents,
            change: change.clone(),
            content: content.clone(),
            agent: req.agent.clone(),
            message: req.message.clone(),
            at,
            op: None,
            status: PatchStatus::Pending,
            depends_on,
            implements,
            wit_binding,
            status_op: 0,
            order,
            placement: req.position.clone(),
        };
        let primary = (
            PointerMove {
                pointer: loc.pointer.clone(),
                before: loc.tip_hash(),
                after: Some(patch.clone()),
            },
            loc.guard(),
        );
        let (moves, guards): (Vec<_>, Vec<_>) =
            claims.into_iter().chain([primary]).chain(releases).unzip();
        let op = NewOp {
            at,
            agent: req.agent.clone(),
            kind: OpKind::Apply(patch.clone()),
            moves,
            intent: Intent { guards, patches: vec![rec], conflicts: vec![], effects: vec![] },
        };
        let Some(entry) = self.execute(ws, op).await? else { return Ok(None) };
        let from = match (req.read_at, &req.parent, change) {
            (Some(r), _, _) => Some(r),
            (None, Some(parent), c) if !matches!(c, Change::Create(_)) => {
                self.graph.patch(ws, parent).await?.and_then(|p| p.op)
            }
            _ => None,
        };
        let commuted_with = match from {
            Some(from) => {
                self.commuted_with(ws, &loc.key, &symbol.component, from, entry.id()).await?
            }
            None => vec![],
        };
        Ok(Some(CommitResult {
            patch: patch.clone(),
            op: entry.id(),
            outcome: if commuted_with.is_empty() {
                PatchOutcome::Applied
            } else {
                PatchOutcome::Commuted
            },
            tip: content.is_some().then_some(patch),
            commuted_with,
            conflict: None,
        }))
    }

    /// Patches that moved another symbol of `component`'s tip in ops after
    /// `from` that had landed by now — any id but `ours`: an op logged after
    /// ours may have landed before it (ids are assigned when an op is logged,
    /// not when it lands).
    async fn commuted_with(
        &self,
        ws: &str,
        key: &str,
        component: &str,
        from: OpId,
        ours: OpId,
    ) -> Result<Vec<Hash>> {
        let mut out = Vec::new();
        let mut after = from;
        loop {
            let page = self.log.list(ws, Some(after), 256).await?;
            if page.is_empty() {
                break;
            }
            for op in page {
                after = op.id();
                if op.id() == ours
                    || !matches!(op.entry.kind, OpKind::Apply(_) | OpKind::Resolve(_))
                {
                    continue;
                }
                let state = match op.state {
                    OpState::Pending => self.settle(ws, op.id()).await?,
                    s => s,
                };
                if state != OpState::Committed {
                    continue;
                }
                for m in op.entry.moves.iter().filter(|m| !is_name_pointer(&m.pointer)) {
                    let (Some(h), true) = (&m.after, m.before != m.after) else { continue };
                    let p = match op.intent.patches.iter().find(|p| &p.hash == h) {
                        Some(p) => Some(p.clone()),
                        None => self.graph.patch(ws, h).await?,
                    };
                    if let Some(p) = p {
                        if p.key != key && p.symbol.component == component && !out.contains(h) {
                            out.push(h.clone());
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Record a conflict `left` vs `patch`. `Ok(None)`: lost the CAS.
    #[allow(clippy::too_many_arguments)]
    async fn open_conflict(
        &self,
        req: &PatchRequest,
        loc: &Located,
        change: &Change,
        parent_record: Option<&PatchRecord>,
        patch: Hash,
        parents: Vec<Hash>,
        base: Option<Hash>,
        left: Hash,
    ) -> Result<Option<CommitResult>> {
        let ws = req.workspace.as_str();
        let tip = loc.tip.as_ref().expect("a conflict has a tip");
        let (symbol, content, depends_on, implements) = match change {
            Change::Create(c) | Change::Replace(c) => (
                req.symbol.clone(),
                Some(c.clone()),
                req.depends_on.clone(),
                req.implements.clone(),
            ),
            Change::Delete => (req.symbol.clone(), None, vec![], vec![]),
            Change::Rename(name) => {
                let p = parent_record.unwrap_or(tip);
                (
                    p.symbol.renamed(name),
                    p.content.clone(),
                    p.depends_on.clone(),
                    p.implements.clone(),
                )
            }
            Change::Move(_) => {
                let p = parent_record.unwrap_or(tip);
                (p.symbol.clone(), p.content.clone(), p.depends_on.clone(), p.implements.clone())
            }
        };
        let cid = conflict_id(&left, &patch);
        if let Some(existing) = self.graph.conflict(ws, &cid).await? {
            if existing.state == ConflictState::Open && existing.opened_at != 0 {
                // Another writer already opened exactly this one.
                return Ok(Some(CommitResult {
                    patch,
                    op: existing.opened_at,
                    outcome: PatchOutcome::Conflicted,
                    tip: loc.live_tip(),
                    commuted_with: vec![],
                    conflict: Some(cid),
                }));
            }
        }
        let at = (self.clock)();
        let rec = PatchRecord {
            hash: patch.clone(),
            key: loc.key.clone(),
            symbol,
            parents,
            change: change.clone(),
            content: content.clone(),
            agent: req.agent.clone(),
            message: req.message.clone(),
            at,
            op: None,
            status: PatchStatus::Pending,
            depends_on,
            implements,
            wit_binding: req.wit_binding.clone(),
            status_op: 0,
            order: tip.order.clone(),
            placement: req.position.clone(),
        };
        let record = ConflictRecord {
            id: cid.clone(),
            key: loc.key.clone(),
            symbol: tip.symbol.clone(),
            base,
            left: SideRecord {
                patch: left.clone(),
                agent: tip.agent.clone(),
                content: tip.content.clone(),
            },
            right: SideRecord { patch: patch.clone(), agent: req.agent.clone(), content },
            state: ConflictState::Open,
            opened_at: 0,
            resolved_by: None,
            pending: vec![],
            state_op: 0,
        };
        let op = NewOp {
            at,
            agent: req.agent.clone(),
            kind: OpKind::Apply(patch.clone()),
            // The no-op move: serialises this conflict against any applier.
            moves: vec![PointerMove {
                pointer: loc.pointer.clone(),
                before: Some(left.clone()),
                after: Some(left.clone()),
            }],
            intent: Intent {
                guards: vec![loc.guard()],
                patches: vec![rec],
                conflicts: vec![record],
                effects: vec![ConflictEffect {
                    conflict: cid.clone(),
                    before: ConflictState::Abandoned,
                    after: ConflictState::Open,
                    resolved_by_before: None,
                    resolved_by_after: None,
                }],
            },
        };
        let Some(entry) = self.execute(ws, op).await? else { return Ok(None) };
        Ok(Some(CommitResult {
            patch,
            op: entry.id(),
            outcome: PatchOutcome::Conflicted,
            tip: loc.live_tip(),
            commuted_with: vec![],
            conflict: Some(cid),
        }))
    }

    // ---- resolve-conflict ----------------------------------------------------

    /// Settle an open conflict: the tip moves from `left` to a resolution patch
    /// whose parents are `[left, right]`.
    pub async fn resolve_conflict(&self, req: ResolutionRequest) -> Result<CommitResult> {
        let ws = req.workspace.as_str();
        let change = match &req.resolution {
            Transformation::Replace(c) => Change::Replace(self.content_hash(c).await?),
            Transformation::Delete => Change::Delete,
            Transformation::Create(_) | Transformation::Rename(_) | Transformation::Move(_) => {
                return Err(VcsError::Invalid("a resolution is `replace` or `delete`".into()));
            }
        };
        let mut last = None;
        let find = || async {
            self.graph
                .conflict(ws, &req.conflict)
                .await?
                .ok_or_else(|| VcsError::NotFound(format!("conflict {}", req.conflict)))
        };
        for _ in 0..self.cas_tries {
            // The tip first: reading it finishes whatever op last wrote it — a
            // resolve of this very conflict that crashed before marking it, say.
            let t = self.read_tip(ws, &find().await?.key).await?;
            let mut c = find().await?;
            if c.opened_at == 0 && c.state == ConflictState::Open && !c.pending.is_empty() {
                // Written ahead by ops not yet finished: finish them first, so a
                // conflict whose opening aborted is not resolved.
                for j in c.pending.clone() {
                    self.settle(ws, j).await?;
                }
                c = self
                    .graph
                    .conflict(ws, &req.conflict)
                    .await?
                    .ok_or_else(|| VcsError::NotFound(format!("conflict {}", req.conflict)))?;
            }
            let parents = vec![c.left.patch.clone(), c.right.patch.clone()];
            let res = patch_hash(&c.key, &parents, &change, None);
            match c.state {
                ConflictState::Open => {}
                ConflictState::Resolved if c.resolved_by.as_ref() == Some(&res) => {
                    let rec = self.patch_required(ws, &res).await?;
                    return Ok(CommitResult {
                        patch: res,
                        op: rec.op.unwrap_or(0),
                        outcome: PatchOutcome::Duplicate,
                        tip: self.live_tip_of(ws, &c.key).await?,
                        commuted_with: vec![],
                        conflict: None,
                    });
                }
                ConflictState::Resolved => {
                    return Err(VcsError::Invalid(format!(
                        "conflict {} is already resolved by {}",
                        c.id,
                        c.resolved_by.unwrap_or_default()
                    )));
                }
                ConflictState::Abandoned => {
                    return Err(VcsError::Invalid(format!("conflict {} was abandoned", c.id)));
                }
            }
            if t.rev.is_none() {
                return Err(VcsError::Storage(format!(
                    "conflict {} names a symbol with no pointer",
                    c.id
                )));
            }
            let tip = t.tip.as_ref().map(|r| r.hash.clone());
            if tip.as_ref() != Some(&c.left.patch) {
                // Reading the tip settles the op that wrote it — which may have
                // been this very resolution, crashed before finishing. Look again.
                let now = self.graph.conflict(ws, &c.id).await?;
                if now.as_ref().is_some_and(|n| n.state != c.state) {
                    continue;
                }
                return Err(VcsError::ConcurrentModification(CasFailure {
                    pointer: t.pointer,
                    expected: Some(c.left.patch.clone()),
                    actual: tip,
                }));
            }
            let left = self.patch_required(ws, &c.left.patch).await?;
            let right = self.patch_required(ws, &c.right.patch).await?;
            let content = match &change {
                Change::Replace(h) => Some(h.clone()),
                _ => None,
            };
            let union = |a: &[SymbolId], b: &[SymbolId]| -> Vec<SymbolId> {
                let s: BTreeSet<SymbolId> = a.iter().chain(b).cloned().collect();
                s.into_iter().collect()
            };
            let at = (self.clock)();
            let wit_binding = left.wit_binding.clone().or_else(|| right.wit_binding.clone());
            let rec = PatchRecord {
                hash: res.clone(),
                key: c.key.clone(),
                symbol: left.symbol.clone(),
                parents,
                change: change.clone(),
                content: content.clone(),
                agent: req.agent.clone(),
                message: req.message.clone(),
                at,
                op: None,
                status: PatchStatus::Pending,
                depends_on: if content.is_some() {
                    union(&left.depends_on, &right.depends_on)
                } else {
                    vec![]
                },
                implements: if content.is_some() {
                    union(&left.implements, &right.implements)
                } else {
                    vec![]
                },
                wit_binding,
                status_op: 0,
                order: left.order.clone(),
                placement: None,
            };
            let siblings: Vec<ConflictRecord> = self
                .graph
                .open_conflicts_for(ws, &c.key)
                .await?
                .into_iter()
                .filter(|s| s.id != c.id)
                .collect();
            let repointed: Vec<ConflictRecord> = siblings
                .iter()
                .map(|s| ConflictRecord {
                    id: conflict_id(&res, &s.right.patch),
                    key: c.key.clone(),
                    symbol: left.symbol.clone(),
                    base: s.base.clone(),
                    left: SideRecord {
                        patch: res.clone(),
                        agent: req.agent.clone(),
                        content: content.clone(),
                    },
                    right: s.right.clone(),
                    state: ConflictState::Open,
                    opened_at: 0,
                    resolved_by: None,
                    pending: vec![],
                    state_op: 0,
                })
                .collect();
            let effect = |conflict: &str, before, after, rb: Option<Hash>| ConflictEffect {
                conflict: conflict.to_string(),
                before,
                after,
                resolved_by_before: None,
                resolved_by_after: rb,
            };
            let mut effects = vec![effect(
                &c.id,
                ConflictState::Open,
                ConflictState::Resolved,
                Some(res.clone()),
            )];
            for s in &siblings {
                effects.push(effect(&s.id, ConflictState::Open, ConflictState::Abandoned, None));
            }
            for r in &repointed {
                effects.push(effect(&r.id, ConflictState::Abandoned, ConflictState::Open, None));
            }
            let mut moves = vec![PointerMove {
                pointer: t.pointer.clone(),
                before: tip.clone(),
                after: Some(res.clone()),
            }];
            let mut guards = vec![Guard { rev: t.rev, op: t.op }];
            if content.is_none() {
                // Resolved by deleting: the name is free again, as after a delete.
                let np = name_pointer(ws, &left.symbol);
                if let Some((v, r)) = self.pointers.get(&np).await? {
                    if v.value.as_deref() == Some(c.key.as_str()) {
                        moves.push(PointerMove {
                            pointer: np,
                            before: Some(c.key.clone()),
                            after: None,
                        });
                        guards.push(Guard { rev: Some(r), op: v.op });
                    }
                }
            }
            let op = NewOp {
                at,
                agent: req.agent.clone(),
                kind: OpKind::Resolve(c.id.clone()),
                moves,
                intent: Intent { guards, patches: vec![rec], conflicts: repointed, effects },
            };
            match self.execute(ws, op).await? {
                Some(entry) => {
                    return Ok(CommitResult {
                        patch: res.clone(),
                        op: entry.id(),
                        outcome: PatchOutcome::Applied,
                        tip: content.is_some().then_some(res),
                        commuted_with: vec![],
                        conflict: None,
                    })
                }
                None => {
                    last = Some((t.pointer, tip));
                    yield_now().await;
                }
            }
        }
        Err(self.exhausted(last).await)
    }

    async fn live_tip_of(&self, ws: &str, key: &str) -> Result<Option<Hash>> {
        let t = self.read_tip(ws, key).await?;
        Ok(t.tip.filter(|r| !r.is_delete()).map(|r| r.hash))
    }

    // ---- revert-op -----------------------------------------------------------

    /// Undo `op` by logging and carrying out its inverse (see the module notes).
    pub async fn revert_op(&self, ws: &str, op: OpId, by: Agent) -> Result<OpEntry> {
        let original = self
            .log
            .get(ws, op)
            .await?
            .ok_or_else(|| VcsError::NotFound(format!("op {op} in {ws}")))?;
        match self.settle(ws, op).await? {
            OpState::Committed => {}
            OpState::Aborted => {
                return Err(VcsError::NotFound(format!("op {op} in {ws} was aborted")));
            }
            OpState::Pending => {
                let m = &original.entry.moves[0];
                return Err(VcsError::ConcurrentModification(CasFailure {
                    pointer: m.pointer.clone(),
                    expected: m.after.clone(),
                    actual: None,
                }));
            }
        }
        if original.entry.moves.is_empty() {
            return Err(VcsError::Invalid(format!("op {op} moved nothing")));
        }
        let moves: BTreeMap<&str, &PointerMove> =
            original.entry.moves.iter().map(|m| (m.pointer.as_str(), m)).collect();
        let mut last = None;
        for _ in 0..self.cas_tries {
            // 1. Nothing later may have touched these pointers (in flight counts).
            let mut after = op;
            loop {
                let page = self.log.list(ws, Some(after), 256).await?;
                if page.is_empty() {
                    break;
                }
                for e in &page {
                    after = e.id();
                    let Some(m) = e.entry.moves.iter().find_map(|m| moves.get(m.pointer.as_str()))
                    else {
                        continue;
                    };
                    let state = match e.state {
                        OpState::Pending => self.settle(ws, e.id()).await?,
                        s => s,
                    };
                    if state == OpState::Aborted {
                        continue;
                    }
                    let actual = self.pointers.get(&m.pointer).await?.and_then(|(v, _)| v.value);
                    return Err(VcsError::ConcurrentModification(CasFailure {
                        pointer: m.pointer.clone(),
                        expected: m.after.clone(),
                        actual,
                    }));
                }
            }
            // 2. Every pointer holds the op's `after` now.
            let mut guards = Vec::with_capacity(original.entry.moves.len());
            for m in &original.entry.moves {
                let (value, guard) = match self.pointers.get(&m.pointer).await? {
                    Some((v, r)) => (v.value.clone(), Guard { rev: Some(r), op: v.op }),
                    None => (None, Guard::default()),
                };
                if value != m.after {
                    return Err(VcsError::ConcurrentModification(CasFailure {
                        pointer: m.pointer.clone(),
                        expected: m.after.clone(),
                        actual: value,
                    }));
                }
                guards.push(guard);
            }
            // 3. Log the inverse, and carry it out.
            let moves: Vec<PointerMove> = original
                .entry
                .moves
                .iter()
                .map(|m| PointerMove {
                    pointer: m.pointer.clone(),
                    before: m.after.clone(),
                    after: m.before.clone(),
                })
                .collect();
            let effects = original.intent.effects.iter().map(ConflictEffect::inverse).collect();
            let new = NewOp {
                at: (self.clock)(),
                agent: by.clone(),
                kind: OpKind::Revert(op),
                moves,
                intent: Intent { guards, patches: vec![], conflicts: vec![], effects },
            };
            match self.execute(ws, new).await? {
                Some(done) => return Ok(done.entry),
                None => {
                    let m = &original.entry.moves[0];
                    last = Some((m.pointer.clone(), m.after.clone()));
                    yield_now().await;
                }
            }
        }
        Err(self.exhausted(last).await)
    }

    // ---- query-symbol --------------------------------------------------------

    pub async fn query_symbol(&self, ws: &str, query: SymbolQuery) -> Result<Vec<SymbolView>> {
        // BEFORE reading anything: every op at or below it is then reflected.
        let as_of = self.oplog_head(ws).await?;
        match query {
            SymbolQuery::Symbol(id) => {
                let loc = self.locate(ws, &id).await?;
                match loc.tip {
                    Some(t) if !t.is_delete() => Ok(vec![self.view(ws, &loc.key, t, as_of).await?]),
                    _ => Err(VcsError::SymbolNotFound(id)),
                }
            }
            SymbolQuery::Component(component) => {
                let mut out = Vec::new();
                for (_, _, key, rec) in self.component_order(ws, &component, None).await? {
                    out.push(self.view(ws, &key, rec, as_of).await?);
                }
                Ok(out)
            }
        }
    }

    /// The live symbols of a component, as `(path, order key, symbol key, tip)`,
    /// in file order.
    async fn component_order(
        &self,
        ws: &str,
        component: &str,
        path: Option<&str>,
    ) -> Result<Vec<(String, String, String, PatchRecord)>> {
        let mut live = Vec::new();
        for s in self.graph.symbols_in_component(ws, component).await? {
            if path.is_some_and(|p| p != s.path) {
                continue;
            }
            let t = self.read_tip(ws, &s.key).await?;
            let Some(rec) = t.tip.filter(|r| !r.is_delete()) else { continue };
            let position = match s.position {
                Some(p) => Some(p),
                // Finished since the listing was read (read_tip finishes a
                // pending tip): read the position again.
                None => self.graph.symbol(ws, &s.key).await?.and_then(|s| s.position),
            };
            let key =
                rec.order.clone().unwrap_or_else(|| order::derived(position.unwrap_or(OpId::MAX)));
            live.push((s.path.clone(), key, s.key.clone(), rec));
        }
        live.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
        Ok(live)
    }

    /// The order key `placement` puts `sym` (filed at `self_key`) at, among the
    /// OTHER live symbols of its file.
    async fn resolve_order(
        &self,
        ws: &str,
        sym: &SymbolId,
        self_key: &str,
        placement: &Placement,
    ) -> Result<String> {
        let file: Vec<(String, SymbolId)> = self
            .component_order(ws, &sym.component, Some(&sym.path))
            .await?
            .into_iter()
            .filter(|(_, _, k, _)| k != self_key)
            .map(|(_, order, _, rec)| (order, rec.symbol))
            .collect();
        // Everything placed stays below the implicit key of any op yet to come,
        // so unplaced creates keep appending.
        let next = order::derived(self.log.latest(ws).await?.unwrap_or(0) + 1);
        let ceiling = |lo: &str| (next.as_str() > lo).then(|| next.clone());
        let find = |x: &SymbolId| -> Result<usize> {
            if x.component != sym.component || x.path != sym.path {
                return Err(VcsError::Invalid(format!(
                    "{x} is not in {}:{}, so {sym} cannot be placed next to it",
                    sym.component, sym.path
                )));
            }
            if x.name == sym.name && x.kind == sym.kind {
                return Err(VcsError::Invalid(format!("{sym} cannot be placed next to itself")));
            }
            file.iter().position(|(_, s)| s == x).ok_or_else(|| VcsError::SymbolNotFound(x.clone()))
        };
        Ok(match placement {
            Placement::First => match file.first() {
                Some((hi, _)) => order::between(None, Some(hi)),
                None => order::between(None, Some(&next)),
            },
            Placement::Last => match file.last() {
                Some((lo, _)) => order::between(Some(lo), ceiling(lo).as_deref()),
                None => order::between(None, Some(&next)),
            },
            Placement::After(x) => {
                let i = find(x)?;
                let lo = &file[i].0;
                match file.get(i + 1) {
                    // Tied with its successor: join the tie (see `order`).
                    Some((hi, _)) if hi == lo => lo.clone(),
                    Some((hi, _)) => order::between(Some(lo), Some(hi)),
                    None => order::between(Some(lo), ceiling(lo).as_deref()),
                }
            }
            Placement::Before(x) => {
                let i = find(x)?;
                let hi = &file[i].0;
                match i.checked_sub(1).map(|j| &file[j].0) {
                    Some(lo) if lo == hi => hi.clone(),
                    lo => order::between(lo.map(String::as_str), Some(hi)),
                }
            }
        })
    }

    async fn view(&self, ws: &str, key: &str, tip: PatchRecord, as_of: OpId) -> Result<SymbolView> {
        let content = self.present(tip.content.as_deref().unwrap_or_default()).await?;
        let mut dependents = BTreeSet::new();
        for h in self.graph.dependents(ws, &tip.symbol).await? {
            let Some(p) = self.graph.patch(ws, &h).await? else { continue };
            if p.is_delete() {
                continue;
            }
            // Only a dependent whose CURRENT content uses this symbol counts.
            if let Some((v, _)) = self.pointers.get(&symbol_pointer(ws, &p.key)).await? {
                if v.value.as_ref() == Some(&h) {
                    dependents.insert(p.symbol.clone());
                }
            }
        }
        let open_conflicts =
            self.graph.open_conflicts_for(ws, key).await?.into_iter().map(|c| c.id).collect();
        Ok(SymbolView {
            id: tip.symbol.clone(),
            tip: tip.hash.clone(),
            content,
            author: tip.agent.clone(),
            wit_binding: tip.wit_binding.clone(),
            depends_on: tip.depends_on.clone(),
            dependents: dependents.into_iter().collect(),
            implements: tip.implements.clone(),
            open_conflicts,
            as_of,
        })
    }

    /// A blob as the contract returns content: inline when small and UTF-8.
    async fn present(&self, h: &str) -> Result<Content> {
        let bytes = self.blobs.get(h).await?.ok_or_else(|| {
            VcsError::Storage(format!("blob {h} is missing from the object store"))
        })?;
        if bytes.len() <= INLINE_MAX {
            if let Ok(s) = String::from_utf8(bytes) {
                return Ok(Content::Inline(s));
            }
        }
        Ok(Content::Blob(h.to_string()))
    }

    // ---- snapshot-export -----------------------------------------------------

    /// Flatten a component into files. Within a file, live symbols are laid out
    /// in order-key order (see the module notes on positions). Each symbol's
    /// content is emitted verbatim; a `\n` is inserted between two symbols only
    /// when the first does not already end in one. A `kind: file` symbol is just
    /// a symbol whose content is the whole file.
    ///
    /// `at` is exactly the state after that op: the snapshot is read while no op
    /// is in flight, and re-read if one was logged meanwhile (every pointer write
    /// is logged before it happens).
    pub async fn snapshot_export(&self, ws: &str, component: &str) -> Result<Snapshot> {
        for _ in 0..SNAPSHOT_TRIES {
            let before = self.log.latest(ws).await?.unwrap_or(0);
            if self.oplog_head(ws).await? < before {
                yield_now().await; // an op is in flight
                continue;
            }
            let mut open: Vec<ConflictId> = self
                .graph
                .conflicts(ws, Some(ConflictState::Open))
                .await?
                .into_iter()
                .filter(|c| c.symbol.component == component)
                .map(|c| c.id)
                .collect();
            if !open.is_empty() {
                open.sort();
                return Err(VcsError::UnresolvedConflict(open));
            }
            let pieces = self.component_order(ws, component, None).await?;
            let after = self.log.latest(ws).await?.unwrap_or(0);
            if before != after {
                continue;
            }
            let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for (path, _, _, rec) in pieces {
                let h = rec.content.expect("live");
                let bytes = self.blobs.get(&h).await?.ok_or_else(|| {
                    VcsError::Storage(format!("blob {h} is missing from the object store"))
                })?;
                let file = files.entry(path).or_default();
                if !file.is_empty() && !file.ends_with(b"\n") {
                    file.push(b'\n');
                }
                file.extend_from_slice(&bytes);
            }
            let mut entries = Vec::with_capacity(files.len());
            let mut tree = Vec::with_capacity(files.len());
            for (path, bytes) in files {
                let blob = self.blobs.put(bytes.clone()).await?;
                entries.push(TreeEntry { path: path.clone(), blob, executable: false });
                tree.push((path, bytes, false));
            }
            let git_tree = Some(git::tree_id(&tree)?);
            return Ok(Snapshot {
                workspace: ws.to_string(),
                component: component.to_string(),
                at: after,
                entries,
                git_tree,
            });
        }
        Err(VcsError::ConcurrentModification(CasFailure {
            pointer: format!("oplog/{}/head", escape(ws)),
            expected: None,
            actual: None,
        }))
    }

    // ---- list-conflicts / oplog ----------------------------------------------

    pub async fn list_conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> Result<Vec<Conflict>> {
        let mut recs = self.graph.conflicts(ws, state).await?;
        recs.sort_by(|a, b| (a.opened_at, &a.id).cmp(&(b.opened_at, &b.id)));
        let mut out = Vec::with_capacity(recs.len());
        for c in recs {
            out.push(Conflict {
                id: c.id,
                workspace: ws.to_string(),
                symbol: c.symbol,
                base: c.base,
                left: self.side(c.left).await?,
                right: self.side(c.right).await?,
                state: c.state,
                opened_at: c.opened_at,
                resolved_by: c.resolved_by,
            });
        }
        Ok(out)
    }

    async fn side(&self, s: SideRecord) -> Result<ConflictSide> {
        let content = match &s.content {
            Some(h) => Some(self.present(h).await?),
            None => None,
        };
        Ok(ConflictSide { patch: s.patch, agent: s.agent, content })
    }

    /// The committed ops after `after`, oldest first, at most `limit`. Aborted
    /// ops are skipped; a pending one is settled, and the listing stops at the
    /// first that is still in flight — so a caller paging by the last id it got
    /// never skips an op that commits later.
    pub async fn oplog(&self, ws: &str, after: Option<OpId>, limit: u32) -> Result<Vec<OpEntry>> {
        let mut out = Vec::new();
        let mut cursor = after.unwrap_or(0);
        while out.len() < limit as usize {
            let page = self.log.list(ws, Some(cursor), 256).await?;
            if page.is_empty() {
                break;
            }
            for op in page {
                cursor = op.id();
                let state = match op.state {
                    OpState::Pending => self.settle(ws, op.id()).await?,
                    s => s,
                };
                match state {
                    OpState::Committed => out.push(op.entry),
                    OpState::Aborted => {}
                    OpState::Pending => return Ok(out),
                }
                if out.len() == limit as usize {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// The settled head: the highest id at and below which every op is
    /// committed or aborted (0 for an empty log). What an agent's view reflects,
    /// and what it passes back as `read-at`.
    pub async fn oplog_head(&self, ws: &str) -> Result<OpId> {
        let start = self.log.settled(ws).await?;
        let mut s = start;
        while let Some(op) = self.log.get(ws, s + 1).await? {
            let state = match op.state {
                OpState::Pending => self.settle(ws, op.id()).await?,
                st => st,
            };
            if state == OpState::Pending {
                break;
            }
            s += 1;
        }
        if s > start {
            self.log.advance_settled(ws, s).await?;
        }
        Ok(s)
    }
}

fn validate_request(req: &PatchRequest) -> Result<()> {
    if req.workspace.is_empty() {
        return Err(VcsError::Invalid("empty workspace".into()));
    }
    let s = &req.symbol;
    if s.component.is_empty() || s.name.is_empty() {
        return Err(VcsError::Invalid(format!("symbol {s} needs a component and a name")));
    }
    git::validate_path(&s.path)?;
    if let Some(p) = &req.parent {
        if !is_hash(p) {
            return Err(VcsError::Invalid(format!(
                "parent {p:?} is not 64 lower-case hex characters"
            )));
        }
    }
    if req.position.is_some() && !matches!(req.change, Transformation::Create(_)) {
        return Err(VcsError::Invalid(
            "`position` places a `create`; to re-place a symbol, `move` it".into(),
        ));
    }
    Ok(())
}
