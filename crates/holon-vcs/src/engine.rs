//! The seven `holon:vcs/code-store` operations, over the four storage traits.
//!
//! # The write path, and why it is in this order
//!
//! Every mutation is: **read** the symbol's tip pointer (value and revision) →
//! read the graph state that matters (open conflicts, patch records) → [`decide`]
//! → write the new patch / conflict record to the graph → **compare-and-set** the
//! tip pointer against the revision read → append the op → update the graph's
//! mirrors. When the CAS loses, nothing has been published — re-read, re-decide,
//! bounded by [`Engine::with_cas_tries`] (default [`CAS_TRIES`]); a caller that
//! exhausts it gets `concurrent-modification`.
//!
//! **A conflict also CASes the tip — to the value it already has.** That bumps the
//! revision, and it is what makes "is there an open conflict?" safe to ask: the
//! conflict record is written *before* that CAS, and an applier reads the open
//! conflicts *after* reading the tip. Either the conflict's CAS happened before the
//! applier's read (so the applier sees the record and refuses with
//! `unresolved-conflict`), or both CAS against the same revision and exactly one
//! lands (the loser re-decides). Without the no-op CAS, an edit could land on top
//! of a tip that a conflict opened a moment earlier still names as its `left`.
//!
//! A conflict record written before its CAS is *uncommitted* (`opened-at: 0`).
//! If the CAS loses, the writer abandons it — atomically, only if it is still
//! uncommitted, so it can never abandon one another writer's CAS committed. A
//! process that dies between the two leaves an uncommitted open conflict; its
//! sides are real patches and its left is still the tip, so it is a genuine
//! disagreement and a resolver settles it like any other.
//!
//! # Outcomes, precisely
//!
//! * `applied` — the tip moved from `parent` to the new patch, and no patch on
//!   another symbol of the same component landed between `parent`'s op and this
//!   one.
//! * `commuted` — the tip moved from `parent` (so `parent` WAS the symbol's tip:
//!   nothing touched this symbol since), AND at least one op strictly between the
//!   op that landed `parent` and this patch's op moved the tip of ANOTHER symbol in
//!   the same component. `commuted-with` lists those patches, oldest first. Ops
//!   that only opened a conflict, and reverts, do not count. Measured from
//!   `parent`'s op because the request carries nothing else: the store cannot know
//!   which of those the agent had already read, so this is "what this edit was
//!   reordered past, at most". A `create` never commutes (it has no parent op).
//!   Computed after this patch's op is appended, over the log prefix before it —
//!   so of two racing edits to two symbols, the one with the later op reports the
//!   earlier one, and the earlier one reports `applied`.
//! * `conflicted` — see [`crate::patch`]. The tip does not move. In an N-way race
//!   from one parent, one edit is `applied` and each of the other N-1 opens its
//!   own conflict `(winner, loser)`: every patch is kept.
//! * `duplicate` — nothing was written; `op` is the op that landed the patch.
//!
//! # Resolution with several open conflicts
//!
//! `resolve-conflict` moves the tip from the conflict's `left` to a resolution
//! whose parents are `[left, right]`. Any OTHER open conflict on that symbol was
//! `(left, other)`; its left is no longer the tip, so in the same op it is
//! abandoned and re-opened as `(resolution, other)` with the same base. A resolver
//! working through an N-way race therefore always merges into the current tip,
//! and no side is dropped.
//!
//! # Revert
//!
//! `revert-op` refuses (`concurrent-modification`, nothing changed) if any
//! pointer the op moved was touched by a LATER op — including a conflict's no-op
//! move, since that conflict names the value as its `left` — or does not hold the
//! op's `after` value now. Otherwise it CASes each pointer from `after` back to
//! `before` in order, appends a `revert(op)` entry whose moves are the inverses,
//! and inverts the conflict state changes the op recorded (so reverting an apply
//! that only opened a conflict abandons it; reverting a resolve re-opens the
//! conflict and undoes the sibling re-pointing). Reverting a revert re-applies.
//!
//! Multi-pointer ops: every op this engine writes moves exactly one pointer, but
//! revert is written for many. All pointers are validated before any is written;
//! if a CAS then loses, the pointers already moved are CASed back (best effort,
//! against the revisions this revert produced) and `concurrent-modification` is
//! returned. If one of those roll-back CASes also loses — another writer moved a
//! pointer this revert had just moved, inside the same call — the error says so
//! and names it; there is no multi-key transaction to prevent that window.
//!
//! # What is not atomic
//!
//! Pointer, oplog and graph are three stores. A process that dies after a tip CAS
//! and before its op append leaves a tip the log does not explain (not
//! revertible, invisible to `commuted`); one that dies before the mirror update
//! leaves a mirror behind (lookups verify against the pointer, so that costs a
//! miss, not a wrong write). Rebuilding mirrors and the log from pointers + patch
//! records is a repair job, not done here.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Result, VcsError};
use crate::git;
use crate::graph::{
    Change, ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SideRecord,
    SymbolUpdate,
};
use crate::model::{
    Agent, CasFailure, CommitResult, Conflict, ConflictId, ConflictSide, ConflictState, Content,
    Hash, OpEntry, OpId, OpKind, PatchOutcome, PatchRequest, PointerMove, ResolutionRequest,
    Snapshot, SymbolId, SymbolQuery, SymbolView, Transformation, TreeEntry,
};
use crate::oplog::{NewOp, OpLog};
use crate::patch::{self, conflict_id, decide, patch_hash, symbol_key, Decision, Inputs};
use crate::store::{escape, is_hash, BlobStore, PointerStore, Revision};

/// How many CAS races one operation may lose before `concurrent-modification`.
/// Every loss is somebody else's landed write, so this bounds starvation.
pub const CAS_TRIES: u32 = 200;
/// Probe indexes tried when a name's natural key is held by a symbol since renamed.
pub const MAX_PROBE: u32 = 64;
/// Content at most this long, and UTF-8, is returned `inline`; larger as `blob`.
pub const INLINE_MAX: usize = 64 * 1024;
/// A snapshot re-reads while the oplog moves under it, this many times.
pub const SNAPSHOT_TRIES: u32 = 16;

/// The pointer a symbol's tip lives at: `ws/<escaped workspace>/sym/<key>`.
pub fn symbol_pointer(ws: &str, key: &str) -> String {
    format!("ws/{}/sym/{key}", escape(ws))
}

fn key_of_pointer(pointer: &str) -> Option<&str> {
    pointer.rsplit_once("/sym/").map(|(_, k)| k)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A symbol located by id: its key, its pointer, and the tip there.
struct Located {
    key: String,
    pointer: String,
    rev: Option<Revision>,
    tip: Option<PatchRecord>,
}

impl Located {
    fn tip_hash(&self) -> Option<Hash> {
        self.tip.as_ref().map(|t| t.hash.clone())
    }
    fn live_tip(&self) -> Option<Hash> {
        self.tip.as_ref().filter(|t| !t.is_delete()).map(|t| t.hash.clone())
    }
}

pub struct Engine<B, P, G, L> {
    blobs: B,
    pointers: P,
    graph: G,
    log: L,
    clock: fn() -> u64,
    cas_tries: u32,
}

impl<B, P, G, L> Engine<B, P, G, L>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    pub fn new(blobs: B, pointers: P, graph: G, log: L) -> Self {
        Engine { blobs, pointers, graph, log, clock: now_ms, cas_tries: CAS_TRIES }
    }

    pub fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_cas_tries(mut self, n: u32) -> Self {
        self.cas_tries = n.max(1);
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
            let h0 = patch::request_hash(&loc.key, req.parent.as_ref(), &change);
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
            let decision = match decide(&Inputs {
                key: &loc.key,
                symbol: &req.symbol,
                parent: req.parent.as_ref(),
                change: &change,
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
                        None => self.log.latest(ws).await?.unwrap_or(0),
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
                None => last = Some((loc.pointer.clone(), tip_hash)),
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
            Ok(v) => v.and_then(|(h, _)| h),
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

    async fn patch_required(&self, ws: &str, h: &str) -> Result<PatchRecord> {
        self.graph.patch(ws, h).await?.ok_or_else(|| {
            VcsError::Storage(format!("a pointer names patch {h}, which the graph does not have"))
        })
    }

    /// Find the key `id` lives at, authoritatively: a candidate matches only if
    /// its tip patch's symbol IS `id`. Falls through to the first free probe slot
    /// (never written, or a tombstone), which is where a create would go.
    async fn locate(&self, ws: &str, id: &SymbolId) -> Result<Located> {
        for key in self.graph.symbols_named(ws, id).await? {
            let pointer = symbol_pointer(ws, &key);
            if let Some((Some(t), rev)) = self.pointers.get(&pointer).await? {
                let rec = self.patch_required(ws, &t).await?;
                if rec.symbol == *id {
                    return Ok(Located { key, pointer, rev: Some(rev), tip: Some(rec) });
                }
            }
        }
        for n in 0..MAX_PROBE {
            let key = symbol_key(id, n);
            let pointer = symbol_pointer(ws, &key);
            match self.pointers.get(&pointer).await? {
                None => return Ok(Located { key, pointer, rev: None, tip: None }),
                Some((None, rev)) => {
                    return Ok(Located { key, pointer, rev: Some(rev), tip: None })
                }
                Some((Some(t), rev)) => {
                    let rec = self.patch_required(ws, &t).await?;
                    if rec.symbol == *id {
                        return Ok(Located { key, pointer, rev: Some(rev), tip: Some(rec) });
                    }
                }
            }
        }
        Err(VcsError::Invalid(format!("more than {MAX_PROBE} symbols have been called {id}")))
    }

    /// Move the tip. `Ok(None)`: lost the CAS, nothing published.
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
        let (symbol, content, depends_on, implements, wit_binding) = match change {
            Change::Create(c) | Change::Replace(c) => (
                req.symbol.clone(),
                Some(c.clone()),
                req.depends_on.clone(),
                req.implements.clone(),
                req.wit_binding.clone().or_else(|| tip.and_then(|t| t.wit_binding.clone())),
            ),
            Change::Delete => (req.symbol.clone(), None, vec![], vec![], None),
            Change::Rename(name) => {
                let t = tip.expect("decide: rename needs a live tip");
                let renamed = t.symbol.renamed(name);
                if renamed == t.symbol {
                    return Err(VcsError::Invalid(format!(
                        "{} is already called {name}",
                        t.symbol
                    )));
                }
                let other = self.locate(ws, &renamed).await?;
                if other.key != loc.key && other.live_tip().is_some() {
                    return Err(VcsError::Invalid(format!("there is already a symbol {renamed}")));
                }
                (
                    renamed,
                    t.content.clone(),
                    t.depends_on.clone(),
                    t.implements.clone(),
                    t.wit_binding.clone(),
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
            wit_binding: wit_binding.clone(),
        };
        self.graph.put_patch(ws, &rec).await?;
        self.graph.ensure_symbol(ws, &loc.key, &symbol).await?;
        if self.pointers.cas(&loc.pointer, loc.rev, Some(&patch)).await?.is_err() {
            return Ok(None);
        }
        let entry = self
            .log
            .append(
                ws,
                NewOp {
                    at,
                    agent: req.agent.clone(),
                    kind: OpKind::Apply(patch.clone()),
                    moves: vec![PointerMove {
                        pointer: loc.pointer.clone(),
                        before: loc.tip_hash(),
                        after: Some(patch.clone()),
                    }],
                },
            )
            .await?;
        self.graph.mark_patch(ws, &patch, PatchStatus::Landed, Some(entry.id)).await?;
        self.graph
            .update_symbol(
                ws,
                &loc.key,
                SymbolUpdate {
                    name: symbol.name.clone(),
                    tip: Some(patch.clone()),
                    deleted: content.is_none(),
                    wit_binding,
                    position_if_unset: Some(entry.id),
                },
            )
            .await?;
        let commuted_with = match (&req.parent, change) {
            (Some(parent), c) if !matches!(c, Change::Create(_)) => {
                self.commuted_with(ws, &loc.key, &symbol.component, parent, entry.id).await?
            }
            _ => vec![],
        };
        Ok(Some(CommitResult {
            patch: patch.clone(),
            op: entry.id,
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

    /// Patches that moved another symbol of `component`'s tip in ops strictly
    /// between `parent`'s op and `ours`.
    async fn commuted_with(
        &self,
        ws: &str,
        key: &str,
        component: &str,
        parent: &str,
        ours: OpId,
    ) -> Result<Vec<Hash>> {
        let Some(from) = self.graph.patch(ws, parent).await?.and_then(|p| p.op) else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        let mut after = from;
        'pages: loop {
            let page = self.log.list(ws, Some(after), 256).await?;
            if page.is_empty() {
                break;
            }
            for e in page {
                if e.id >= ours {
                    break 'pages;
                }
                after = e.id;
                if !matches!(e.kind, OpKind::Apply(_) | OpKind::Resolve(_)) {
                    continue;
                }
                for m in e.moves.iter().filter(|m| m.before != m.after) {
                    let Some(h) = &m.after else { continue };
                    if let Some(p) = self.graph.patch(ws, h).await? {
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
        };
        self.graph.put_patch(ws, &rec).await?;
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
        };
        self.graph.put_conflict(ws, &record).await?;
        // The no-op CAS: serialises this conflict against any applier (module notes).
        if self.pointers.cas(&loc.pointer, loc.rev, Some(&left)).await?.is_err() {
            self.graph.abandon_if_uncommitted(ws, &cid).await?;
            return Ok(None);
        }
        let entry = self
            .log
            .append(
                ws,
                NewOp {
                    at,
                    agent: req.agent.clone(),
                    kind: OpKind::Apply(patch.clone()),
                    moves: vec![PointerMove {
                        pointer: loc.pointer.clone(),
                        before: Some(left.clone()),
                        after: Some(left.clone()),
                    }],
                },
            )
            .await?;
        self.graph.commit_conflict(ws, &cid, entry.id).await?;
        self.graph
            .mark_patch(ws, &patch, PatchStatus::Conflicted(cid.clone()), Some(entry.id))
            .await?;
        self.graph
            .put_effects(
                ws,
                entry.id,
                &[ConflictEffect {
                    conflict: cid.clone(),
                    before: ConflictState::Abandoned,
                    after: ConflictState::Open,
                    resolved_by_before: None,
                    resolved_by_after: None,
                }],
            )
            .await?;
        Ok(Some(CommitResult {
            patch,
            op: entry.id,
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
            Transformation::Create(_) | Transformation::Rename(_) => {
                return Err(VcsError::Invalid("a resolution is `replace` or `delete`".into()));
            }
        };
        let mut last = None;
        for _ in 0..self.cas_tries {
            let c = self
                .graph
                .conflict(ws, &req.conflict)
                .await?
                .ok_or_else(|| VcsError::NotFound(format!("conflict {}", req.conflict)))?;
            let parents = vec![c.left.patch.clone(), c.right.patch.clone()];
            let res = patch_hash(&c.key, &parents, &change);
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
            let pointer = symbol_pointer(ws, &c.key);
            let (tip, rev) = match self.pointers.get(&pointer).await? {
                Some((t, r)) => (t, r),
                None => {
                    return Err(VcsError::Storage(format!(
                        "conflict {} names a symbol with no pointer",
                        c.id
                    )))
                }
            };
            if tip.as_ref() != Some(&c.left.patch) {
                return Err(VcsError::ConcurrentModification(CasFailure {
                    pointer,
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
                wit_binding: wit_binding.clone(),
            };
            self.graph.put_patch(ws, &rec).await?;

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
                })
                .collect();
            for r in &repointed {
                self.graph.put_conflict(ws, r).await?;
            }
            if self.pointers.cas(&pointer, Some(rev), Some(&res)).await?.is_err() {
                for r in &repointed {
                    self.graph.abandon_if_uncommitted(ws, &r.id).await?;
                }
                last = Some((pointer, tip));
                continue;
            }
            let entry = self
                .log
                .append(
                    ws,
                    NewOp {
                        at,
                        agent: req.agent.clone(),
                        kind: OpKind::Resolve(c.id.clone()),
                        moves: vec![PointerMove {
                            pointer: pointer.clone(),
                            before: tip,
                            after: Some(res.clone()),
                        }],
                    },
                )
                .await?;
            let mut effects = vec![ConflictEffect {
                conflict: c.id.clone(),
                before: ConflictState::Open,
                after: ConflictState::Resolved,
                resolved_by_before: None,
                resolved_by_after: Some(res.clone()),
            }];
            self.graph.mark_patch(ws, &res, PatchStatus::Landed, Some(entry.id)).await?;
            self.graph
                .set_conflict_state(ws, &c.id, ConflictState::Resolved, Some(res.clone()))
                .await?;
            for s in &siblings {
                self.graph.set_conflict_state(ws, &s.id, ConflictState::Abandoned, None).await?;
                effects.push(ConflictEffect {
                    conflict: s.id.clone(),
                    before: ConflictState::Open,
                    after: ConflictState::Abandoned,
                    resolved_by_before: None,
                    resolved_by_after: None,
                });
            }
            for r in &repointed {
                self.graph.commit_conflict(ws, &r.id, entry.id).await?;
                self.graph
                    .mark_patch(ws, &r.right.patch, PatchStatus::Conflicted(r.id.clone()), None)
                    .await?;
                effects.push(ConflictEffect {
                    conflict: r.id.clone(),
                    before: ConflictState::Abandoned,
                    after: ConflictState::Open,
                    resolved_by_before: None,
                    resolved_by_after: None,
                });
            }
            self.graph.put_effects(ws, entry.id, &effects).await?;
            self.graph
                .update_symbol(
                    ws,
                    &c.key,
                    SymbolUpdate {
                        name: left.symbol.name.clone(),
                        tip: Some(res.clone()),
                        deleted: content.is_none(),
                        wit_binding,
                        position_if_unset: None,
                    },
                )
                .await?;
            return Ok(CommitResult {
                patch: res.clone(),
                op: entry.id,
                outcome: PatchOutcome::Applied,
                tip: content.is_some().then_some(res),
                commuted_with: vec![],
                conflict: None,
            });
        }
        Err(self.exhausted(last).await)
    }

    async fn live_tip_of(&self, ws: &str, key: &str) -> Result<Option<Hash>> {
        let Some((Some(t), _)) = self.pointers.get(&symbol_pointer(ws, key)).await? else {
            return Ok(None);
        };
        let rec = self.patch_required(ws, &t).await?;
        Ok((!rec.is_delete()).then_some(t))
    }

    // ---- revert-op -----------------------------------------------------------

    /// Undo `op` by appending its inverse (see the module notes).
    pub async fn revert_op(&self, ws: &str, op: OpId, by: Agent) -> Result<OpEntry> {
        let entry = self
            .log
            .get(ws, op)
            .await?
            .ok_or_else(|| VcsError::NotFound(format!("op {op} in {ws}")))?;
        if entry.moves.is_empty() {
            return Err(VcsError::Invalid(format!("op {op} moved nothing")));
        }
        let pointers: BTreeMap<&str, &PointerMove> =
            entry.moves.iter().map(|m| (m.pointer.as_str(), m)).collect();

        // 1. Nothing later may have touched these pointers.
        let mut after = op;
        loop {
            let page = self.log.list(ws, Some(after), 256).await?;
            if page.is_empty() {
                break;
            }
            for e in &page {
                after = e.id;
                if let Some(m) = e.moves.iter().find_map(|m| pointers.get(m.pointer.as_str())) {
                    let actual = self.pointers.get(&m.pointer).await?.and_then(|(v, _)| v);
                    return Err(VcsError::ConcurrentModification(CasFailure {
                        pointer: m.pointer.clone(),
                        expected: m.after.clone(),
                        actual,
                    }));
                }
            }
        }
        // 2. Every pointer holds the op's `after` now.
        let mut revs = Vec::with_capacity(entry.moves.len());
        for m in &entry.moves {
            let (value, rev) = match self.pointers.get(&m.pointer).await? {
                Some((v, r)) => (v, Some(r)),
                None => (None, None),
            };
            if value != m.after {
                return Err(VcsError::ConcurrentModification(CasFailure {
                    pointer: m.pointer.clone(),
                    expected: m.after.clone(),
                    actual: value,
                }));
            }
            revs.push(rev);
        }
        // 3. CAS each back, rolling back on a lost race.
        let mut done: Vec<(usize, Revision)> = Vec::new();
        for (i, m) in entry.moves.iter().enumerate() {
            let lost = match self.pointers.cas(&m.pointer, revs[i], m.before.as_deref()).await {
                Ok(Ok(r)) => {
                    done.push((i, r));
                    continue;
                }
                Ok(Err(_)) => None,
                Err(e) => Some(e),
            };
            let mut stuck = Vec::new();
            for (j, r) in done.iter().rev() {
                let mj = &entry.moves[*j];
                if !matches!(
                    self.pointers.cas(&mj.pointer, Some(*r), mj.after.as_deref()).await,
                    Ok(Ok(_))
                ) {
                    stuck.push(mj.pointer.clone());
                }
            }
            if !stuck.is_empty() {
                return Err(VcsError::Storage(format!(
                    "revert of op {op} lost a race on {} and could not roll back {stuck:?}; those pointers are reverted",
                    m.pointer
                )));
            }
            if let Some(e) = lost {
                return Err(e);
            }
            let actual = self.pointers.get(&m.pointer).await?.and_then(|(v, _)| v);
            return Err(VcsError::ConcurrentModification(CasFailure {
                pointer: m.pointer.clone(),
                expected: m.after.clone(),
                actual,
            }));
        }
        // 4. Log it, and undo the op's conflict state changes.
        let inverse: Vec<PointerMove> = entry
            .moves
            .iter()
            .map(|m| PointerMove {
                pointer: m.pointer.clone(),
                before: m.after.clone(),
                after: m.before.clone(),
            })
            .collect();
        let new = self
            .log
            .append(
                ws,
                NewOp {
                    at: (self.clock)(),
                    agent: by,
                    kind: OpKind::Revert(op),
                    moves: inverse.clone(),
                },
            )
            .await?;
        let effects: Vec<ConflictEffect> =
            self.graph.effects(ws, op).await?.iter().map(ConflictEffect::inverse).collect();
        for e in &effects {
            self.graph
                .set_conflict_state(ws, &e.conflict, e.after, e.resolved_by_after.clone())
                .await?;
        }
        self.graph.put_effects(ws, new.id, &effects).await?;
        // 5. Mirrors.
        for m in &inverse {
            let Some(key) = key_of_pointer(&m.pointer) else { continue };
            let update = match &m.after {
                Some(h) => {
                    let rec = self.patch_required(ws, h).await?;
                    SymbolUpdate {
                        name: rec.symbol.name.clone(),
                        tip: Some(h.clone()),
                        deleted: rec.is_delete(),
                        wit_binding: rec.wit_binding.clone(),
                        position_if_unset: None,
                    }
                }
                None => {
                    let Some(sym) = self.graph.symbol(ws, key).await? else { continue };
                    SymbolUpdate {
                        name: sym.name,
                        tip: None,
                        deleted: true,
                        wit_binding: None,
                        position_if_unset: None,
                    }
                }
            };
            self.graph.update_symbol(ws, key, update).await?;
        }
        Ok(new)
    }

    // ---- query-symbol --------------------------------------------------------

    pub async fn query_symbol(&self, ws: &str, query: SymbolQuery) -> Result<Vec<SymbolView>> {
        match query {
            SymbolQuery::Symbol(id) => {
                let loc = self.locate(ws, &id).await?;
                match loc.tip {
                    Some(t) if !t.is_delete() => Ok(vec![self.view(ws, &loc.key, t).await?]),
                    _ => Err(VcsError::SymbolNotFound(id)),
                }
            }
            SymbolQuery::Component(component) => {
                let mut live = Vec::new();
                for s in self.graph.symbols_in_component(ws, &component).await? {
                    if let Some((Some(t), _)) =
                        self.pointers.get(&symbol_pointer(ws, &s.key)).await?
                    {
                        let rec = self.patch_required(ws, &t).await?;
                        if !rec.is_delete() {
                            live.push((
                                s.path.clone(),
                                s.position.unwrap_or(OpId::MAX),
                                s.key.clone(),
                                rec,
                            ));
                        }
                    }
                }
                live.sort_by(|a, b| (&a.0, a.1, &a.2).cmp(&(&b.0, b.1, &b.2)));
                let mut out = Vec::with_capacity(live.len());
                for (_, _, key, rec) in live {
                    out.push(self.view(ws, &key, rec).await?);
                }
                Ok(out)
            }
        }
    }

    async fn view(&self, ws: &str, key: &str, tip: PatchRecord) -> Result<SymbolView> {
        let content = self.present(tip.content.as_deref().unwrap_or_default()).await?;
        let mut dependents = BTreeSet::new();
        for h in self.graph.dependents(ws, &tip.symbol).await? {
            let Some(p) = self.graph.patch(ws, &h).await? else { continue };
            if p.is_delete() {
                continue;
            }
            // Only a dependent whose CURRENT content uses this symbol counts.
            if let Some((Some(t), _)) = self.pointers.get(&symbol_pointer(ws, &p.key)).await? {
                if t == h {
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
    /// in creation order (the op that first created each; a rename or a recreate
    /// keeps the place). Each symbol's content is emitted verbatim; a `\n` is
    /// inserted between two symbols only when the first does not already end in
    /// one. A `kind: file` symbol is just a symbol whose content is the whole file.
    pub async fn snapshot_export(&self, ws: &str, component: &str) -> Result<Snapshot> {
        for _ in 0..SNAPSHOT_TRIES {
            let before = self.log.latest(ws).await?.unwrap_or(0);
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
            let mut pieces: Vec<(String, OpId, String, Hash)> = Vec::new();
            for s in self.graph.symbols_in_component(ws, component).await? {
                if let Some((Some(t), _)) = self.pointers.get(&symbol_pointer(ws, &s.key)).await? {
                    let rec = self.patch_required(ws, &t).await?;
                    if let Some(c) = rec.content {
                        pieces.push((
                            s.path.clone(),
                            s.position.unwrap_or(OpId::MAX),
                            s.key.clone(),
                            c,
                        ));
                    }
                }
            }
            let after = self.log.latest(ws).await?.unwrap_or(0);
            if before != after {
                continue;
            }
            pieces.sort_by(|a, b| (&a.0, a.1, &a.2).cmp(&(&b.0, b.1, &b.2)));
            let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for (path, _, _, h) in pieces {
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

    pub async fn oplog(&self, ws: &str, after: Option<OpId>, limit: u32) -> Result<Vec<OpEntry>> {
        self.log.list(ws, after, limit).await
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
    Ok(())
}
