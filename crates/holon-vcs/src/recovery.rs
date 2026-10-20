//! Crash consistency (ADR-0099): intent, write-ahead graph, claims, primary CAS (the commit point),
//! finish. Op `k` landed iff it is on the writer chain from the pointer's current value.

use std::collections::BTreeSet;

use crate::engine::{is_name_pointer, key_of_pointer, symbol_pointer, Engine};
use crate::error::{Result, VcsError};
use crate::graph::{Graph, PatchStatus, SymbolUpdate};
use crate::model::{
    ConflictState, ConsistencyReport, Inconsistency, MissingRecord, OpId, OpKind, PointerMove,
    PointerState, RepairReport, StaleOp,
};
use crate::oplog::{NewOp, OpLog, OpState, StoredOp};
use crate::patch::conflict_id;
use crate::store::{BlobStore, PointerStore, PointerValue};

/// The move that commits an op: the one on a symbol pointer.
pub(crate) fn primary(op: &StoredOp) -> Result<usize> {
    op.entry
        .moves
        .iter()
        .position(|m| !is_name_pointer(&m.pointer))
        .ok_or_else(|| VcsError::Storage(format!("op {} moves no symbol pointer", op.id())))
}

fn is_claim(m: &PointerMove) -> bool {
    is_name_pointer(&m.pointer) && m.after.is_some()
}

fn is_release(m: &PointerMove) -> bool {
    is_name_pointer(&m.pointer) && m.after.is_none()
}

impl<B, P, G, L> Engine<B, P, G, L>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    /// `Ok(None)`: a CAS lost and the op is aborted; `Err` may leave it pending for `settle`.
    pub(crate) async fn execute(&self, ws: &str, new: NewOp) -> Result<Option<StoredOp>> {
        let op = self.log.append(ws, new).await?;
        self.write_ahead(ws, &op).await?;
        let pi = primary(&op)?;
        let order = op
            .entry
            .moves
            .iter()
            .enumerate()
            .filter(|(i, m)| *i != pi && is_claim(m))
            .map(|(i, _)| i)
            .chain([pi]);
        for i in order {
            let m = &op.entry.moves[i];
            let g = &op.intent.guards[i];
            let value = PointerValue::new(m.after.clone(), op.id());
            if self.pointers.cas(&m.pointer, g.rev, &value).await?.is_err() {
                self.roll_back(ws, &op).await?;
                return match self.log.finish(ws, op.id(), OpState::Aborted).await? {
                    OpState::Aborted => Ok(None),
                    s => Err(VcsError::Storage(format!(
                        "op {} lost its CAS on {} but is {s:?}",
                        op.id(),
                        m.pointer
                    ))),
                };
            }
        }
        self.roll_forward(ws, &op).await?;
        self.log.finish(ws, op.id(), OpState::Committed).await?;
        Ok(Some(op))
    }

    /// Step 2, write-ahead; idempotent.
    async fn write_ahead(&self, ws: &str, op: &StoredOp) -> Result<()> {
        let pi = primary(op)?;
        let m = &op.entry.moves[pi];
        for p in &op.intent.patches {
            self.graph.put_patch(ws, p).await?;
        }
        if let (OpKind::Apply(h), true) = (&op.entry.kind, m.before != m.after) {
            if let Some(p) = op.intent.patches.iter().find(|p| &p.hash == h) {
                self.graph.ensure_symbol(ws, &p.key, &p.symbol).await?;
            }
        }
        for c in &op.intent.conflicts {
            self.graph.put_conflict(ws, c, op.id()).await?;
        }
        Ok(())
    }

    /// Step 5; idempotent and monotone, so finishers may run late or twice. The LAST write marks the
    /// tip's patch `landed`: `Engine::read_tip` treats that as a finished tip.
    pub(crate) async fn roll_forward(&self, ws: &str, op: &StoredOp) -> Result<()> {
        self.write_ahead(ws, op).await?;
        let k = op.id();
        let pi = primary(op)?;
        let m = &op.entry.moves[pi];
        let key = key_of_pointer(&m.pointer).unwrap_or_default();
        for e in &op.intent.effects {
            self.graph.apply_conflict_effect(ws, e, k).await?;
        }
        for (i, m) in op.entry.moves.iter().enumerate() {
            if !is_release(m) {
                continue;
            }
            let g = &op.intent.guards[i];
            // Only if it still holds what this op released; anything else is a later op's.
            for _ in 0..self.cas_tries {
                let Some((cur, rev)) = self.pointers.get(&m.pointer).await? else { break };
                if cur.value != m.before || cur.op != g.op {
                    break;
                }
                let value = PointerValue::new(None, k);
                if self.pointers.cas(&m.pointer, Some(rev), &value).await?.is_ok() {
                    break;
                }
            }
        }
        match &op.entry.kind {
            OpKind::Apply(h) if m.before == m.after => {
                let left = m.after.clone().unwrap_or_default();
                let cid = conflict_id(&left, h);
                self.graph.mark_patch(ws, h, PatchStatus::Conflicted(cid), k, true).await?;
            }
            OpKind::Apply(h) => {
                self.mirror(ws, key, m.after.as_deref(), k, Some(k)).await?;
                self.graph.mark_patch(ws, h, PatchStatus::Landed, k, true).await?;
            }
            OpKind::Resolve(_) => {
                for r in &op.intent.conflicts {
                    let status = PatchStatus::Conflicted(r.id.clone());
                    self.graph.mark_patch(ws, &r.right.patch, status, k, false).await?;
                }
                self.mirror(ws, key, m.after.as_deref(), k, None).await?;
                if let Some(res) = &m.after {
                    self.graph.mark_patch(ws, res, PatchStatus::Landed, k, true).await?;
                }
            }
            OpKind::Revert(_) => {
                if m.before != m.after {
                    self.mirror(ws, key, m.after.as_deref(), k, None).await?;
                }
            }
        }
        Ok(())
    }

    /// Undoes a never-landing op's claims (only those still naming it) and uncommitted conflicts.
    pub(crate) async fn roll_back(&self, ws: &str, op: &StoredOp) -> Result<()> {
        let k = op.id();
        for (i, m) in op.entry.moves.iter().enumerate() {
            if !is_claim(m) {
                continue;
            }
            let g = &op.intent.guards[i];
            for _ in 0..self.cas_tries {
                let Some((cur, rev)) = self.pointers.get(&m.pointer).await? else { break };
                if cur.op != Some(k) || cur.value != m.after {
                    break;
                }
                let restored = PointerValue { value: m.before.clone(), op: g.op };
                if self.pointers.cas(&m.pointer, Some(rev), &restored).await?.is_ok() {
                    break;
                }
            }
        }
        for c in &op.intent.conflicts {
            self.graph.abandon_if_uncommitted(ws, &c.id, k).await?;
        }
        Ok(())
    }

    async fn mirror(
        &self,
        ws: &str,
        key: &str,
        after: Option<&str>,
        op: OpId,
        position: Option<OpId>,
    ) -> Result<()> {
        let update = match after {
            Some(h) => {
                let rec = self.patch_required(ws, h).await?;
                SymbolUpdate {
                    name: rec.symbol.name.clone(),
                    tip: Some(h.to_string()),
                    deleted: rec.is_delete(),
                    wit_binding: rec.wit_binding.clone(),
                    position_if_unset: position,
                    op,
                }
            }
            None => {
                let Some(sym) = self.graph.symbol(ws, key).await? else { return Ok(()) };
                SymbolUpdate {
                    name: sym.name,
                    tip: None,
                    deleted: true,
                    wit_binding: None,
                    position_if_unset: None,
                    op,
                }
            }
        };
        self.graph.update_symbol(ws, key, update).await
    }

    /// Walks the writer chain back from the op the pointer names until it passes `k`.
    pub(crate) async fn landed(
        &self,
        ws: &str,
        k: OpId,
        pointer: &str,
        mut names: Option<OpId>,
    ) -> Result<bool> {
        while let Some(j) = names {
            if j == k {
                return Ok(true);
            }
            if j < k {
                return Ok(false);
            }
            let Some(op) = self.log.get(ws, j).await? else { return Ok(false) };
            let next = op.guard(pointer).and_then(|g| g.op);
            if next.is_some_and(|n| n >= j) {
                return Err(VcsError::Storage(format!(
                    "pointer {pointer}: op {j} names op {next:?} as its predecessor"
                )));
            }
            names = next;
        }
        Ok(false)
    }

    /// Rolls forward, aborts, or fences past the lease; `Pending` means still in flight.
    pub(crate) async fn settle(&self, ws: &str, k: OpId) -> Result<OpState> {
        self.settle_with(ws, k, self.lease_ms).await
    }

    pub(crate) async fn settle_with(&self, ws: &str, k: OpId, lease_ms: u64) -> Result<OpState> {
        for _ in 0..self.cas_tries {
            let op = self
                .log
                .get(ws, k)
                .await?
                .ok_or_else(|| VcsError::NotFound(format!("op {k} in {ws}")))?;
            if op.state != OpState::Pending {
                return Ok(op.state);
            }
            let pi = primary(&op)?;
            let m = &op.entry.moves[pi];
            let g = &op.intent.guards[pi];
            let (cur, rev) = match self.pointers.get(&m.pointer).await? {
                Some((v, r)) => (v, Some(r)),
                None => (PointerValue::default(), None),
            };
            if self.landed(ws, k, &m.pointer, cur.op).await? {
                self.roll_forward(ws, &op).await?;
                return self.log.finish(ws, k, OpState::Committed).await;
            }
            if rev != g.rev {
                self.roll_back(ws, &op).await?;
                return self.log.finish(ws, k, OpState::Aborted).await;
            }
            let age = (self.clock)().saturating_sub(op.entry.at);
            if age < lease_ms {
                return Ok(OpState::Pending);
            }
            // Fence: same value, new revision — its CAS can no longer land.
            if self.pointers.cas(&m.pointer, rev, &cur).await?.is_ok() {
                self.roll_back(ws, &op).await?;
                return self.log.finish(ws, k, OpState::Aborted).await;
            }
        }
        Ok(OpState::Pending)
    }

    async fn all_ops(&self, ws: &str) -> Result<Vec<StoredOp>> {
        let mut out = Vec::new();
        loop {
            let page = self.log.list(ws, out.last().map(StoredOp::id), 256).await?;
            if page.is_empty() {
                return Ok(out);
            }
            out.extend(page);
        }
    }

    /// Check a workspace's three stores against each other, changing nothing.
    pub async fn verify(&self, ws: &str) -> Result<ConsistencyReport> {
        let ops = self.all_ops(ws).await?;
        let now = (self.clock)();
        let mut issues = Vec::new();
        let mut in_flight = Vec::new();
        let mut pointers = BTreeSet::new();
        for op in &ops {
            pointers.extend(op.entry.moves.iter().map(|m| m.pointer.clone()));
            if op.state != OpState::Pending {
                continue;
            }
            let age = now.saturating_sub(op.entry.at);
            if age < self.lease_ms {
                in_flight.push(op.id());
                continue;
            }
            let m = &op.entry.moves[primary(op)?];
            let names = self.pointers.get(&m.pointer).await?.and_then(|(v, _)| v.op);
            let landed = self.landed(ws, op.id(), &m.pointer, names).await?;
            issues.push(Inconsistency::StalePending(StaleOp { op: op.id(), age_ms: age, landed }));
        }
        let by_id = |id: OpId| ops.iter().find(|o| o.id() == id);
        for pointer in &pointers {
            let Some((cur, _)) = self.pointers.get(pointer).await? else { continue };
            let state =
                PointerState { pointer: pointer.clone(), value: cur.value.clone(), op: cur.op };
            // A tombstone naming nothing (a fenced never-written key, a rolled-back first claim)
            // is the same as never written.
            let explained = match cur.op.and_then(by_id) {
                None => cur.value.is_none() && cur.op.is_none(),
                Some(o) if o.state == OpState::Committed => {
                    let mv = o.entry.moves.iter().find(|m| &m.pointer == pointer);
                    mv.is_some_and(|m| m.after == cur.value)
                }
                // Past the lease it is reported as stale-pending above.
                Some(o) if o.state == OpState::Pending => true,
                Some(_) => false,
            };
            if !explained {
                issues.push(Inconsistency::UnexplainedPointer(state));
                continue;
            }
            if is_name_pointer(pointer) {
                if let Some(holder) = &cur.value {
                    let t = self.graph_tip(ws, holder).await?;
                    let fits = t.as_ref().is_some_and(|r| {
                        !r.is_delete() && crate::engine::name_pointer(ws, &r.symbol) == *pointer
                    });
                    if !fits
                        && cur.op.and_then(by_id).is_some_and(|o| o.state == OpState::Committed)
                    {
                        issues.push(Inconsistency::StaleName(pointer.clone()));
                    }
                }
                continue;
            }
            let Some(key) = key_of_pointer(pointer) else { continue };
            let Some(h) = &cur.value else { continue };
            let Some(rec) = self.graph.patch(ws, h).await? else {
                issues.push(Inconsistency::MissingPatch(MissingRecord {
                    by: pointer.clone(),
                    hash: h.clone(),
                }));
                continue;
            };
            let committed = cur.op.and_then(by_id).is_some_and(|o| o.state == OpState::Committed);
            if !committed {
                continue;
            }
            let mirror = self.graph.symbol(ws, key).await?;
            let fresh = mirror.as_ref().is_some_and(|s| {
                s.tip.as_ref() == Some(h) && s.deleted == rec.is_delete() && s.position.is_some()
            });
            if !fresh || rec.status == PatchStatus::Pending {
                issues.push(Inconsistency::StaleMirror(key.to_string()));
            }
            if !rec.is_delete() {
                let np = crate::engine::name_pointer(ws, &rec.symbol);
                let reserved = self
                    .pointers
                    .get(&np)
                    .await?
                    .is_some_and(|(v, _)| v.value.as_deref() == Some(key));
                if !reserved {
                    issues.push(Inconsistency::StaleName(np));
                }
            }
        }
        for c in self.graph.conflicts(ws, None).await? {
            for side in [&c.left.patch, &c.right.patch] {
                if self.graph.patch(ws, side).await?.is_none() {
                    issues.push(Inconsistency::DanglingConflict(MissingRecord {
                        by: c.id.clone(),
                        hash: side.clone(),
                    }));
                }
            }
            if c.state != ConflictState::Open {
                continue;
            }
            let pending_live = c.pending.iter().any(|j| {
                by_id(*j).is_some_and(|o| o.state == OpState::Pending && in_flight.contains(j))
            });
            if pending_live {
                continue;
            }
            let tip =
                self.pointers.get(&symbol_pointer(ws, &c.key)).await?.and_then(|(v, _)| v.value);
            if c.opened_at == 0 || tip.as_ref() != Some(&c.left.patch) {
                issues.push(Inconsistency::OrphanConflict(c.id.clone()));
            }
        }
        Ok(ConsistencyReport {
            workspace: ws.to_string(),
            ops: ops.len() as u64,
            in_flight,
            issues,
        })
    }

    async fn graph_tip(&self, ws: &str, key: &str) -> Result<Option<crate::graph::PatchRecord>> {
        let Some((v, _)) = self.pointers.get(&symbol_pointer(ws, key)).await? else {
            return Ok(None);
        };
        match v.value {
            Some(h) => self.graph.patch(ws, &h).await,
            None => Ok(None),
        }
    }

    /// Settles pending ops, then re-finishes committed and re-undoes aborted ones in log order,
    /// which rebuilds lost graph records from the intents and blobs.
    pub async fn repair(&self, ws: &str) -> Result<RepairReport> {
        let before = self.verify(ws).await?;
        let mut rolled_forward = Vec::new();
        let mut aborted = Vec::new();
        for op in self.all_ops(ws).await? {
            match op.state {
                OpState::Pending => match self.settle(ws, op.id()).await? {
                    OpState::Committed => rolled_forward.push(op.id()),
                    OpState::Aborted => aborted.push(op.id()),
                    OpState::Pending => {}
                },
                OpState::Committed => self.roll_forward(ws, &op).await?,
                OpState::Aborted => self.roll_back(ws, &op).await?,
            }
        }
        self.oplog_head(ws).await?; // advances the settled hint
        let after = self.verify(ws).await?;
        let fixed = before.issues.iter().filter(|i| !after.issues.contains(i)).cloned().collect();
        Ok(RepairReport {
            workspace: ws.to_string(),
            rolled_forward,
            aborted,
            fixed,
            remaining: after.issues,
        })
    }
}
