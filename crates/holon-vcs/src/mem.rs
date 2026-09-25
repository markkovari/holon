//! In-memory backends: for tests, and for an engine running inside a component
//! with nothing behind it. A `std::sync::Mutex` per store; no await point is ever
//! held under a lock, so these are fine on any executor (and on wasm32-wasip2).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use crate::error::Result;
use crate::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use crate::model::{ConflictState, Hash, OpId, SymbolId};
use crate::oplog::KvOpLog;
use crate::store::{sha256_hex, BlobStore, CasMismatch, Kv, KvPointers, Revision};

#[derive(Default)]
pub struct MemBlobs {
    blobs: Mutex<HashMap<Hash, Vec<u8>>>,
}

impl MemBlobs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStore for MemBlobs {
    async fn put(&self, bytes: Vec<u8>) -> Result<Hash> {
        let h = sha256_hex(&bytes);
        self.blobs.lock().unwrap().entry(h.clone()).or_insert(bytes);
        Ok(h)
    }
    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.blobs.lock().unwrap().get(hash).cloned())
    }
    async fn contains(&self, hash: &str) -> Result<bool> {
        Ok(self.blobs.lock().unwrap().contains_key(hash))
    }
}

/// A CAS map. Revisions come from one counter across all keys, like a JetStream
/// stream sequence — so a key's revision is never reused, even across keys.
/// The revision counter, and each key's value and revision.
type KvState = (u64, HashMap<String, (Vec<u8>, Revision)>);

#[derive(Default)]
pub struct MemKv {
    inner: Mutex<KvState>,
}

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Kv for MemKv {
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        Ok(self.inner.lock().unwrap().1.get(key).cloned())
    }

    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Vec<u8>,
    ) -> Result<std::result::Result<Revision, CasMismatch>> {
        let mut g = self.inner.lock().unwrap();
        let current = g.1.get(key).map(|(_, r)| *r);
        if current != expected {
            return Ok(Err(CasMismatch { current }));
        }
        g.0 += 1;
        let rev = g.0;
        g.1.insert(key.to_string(), (value, rev));
        Ok(Ok(rev))
    }
}

pub type MemPointers = KvPointers<MemKv>;
pub type MemOpLog = KvOpLog<MemKv>;

pub fn pointers() -> MemPointers {
    KvPointers::new(MemKv::new())
}

pub fn oplog() -> MemOpLog {
    KvOpLog::new(MemKv::new())
}

#[derive(Default)]
struct GraphState {
    symbols: BTreeMap<(String, String), SymbolRecord>,
    patches: HashMap<(String, Hash), PatchRecord>,
    conflicts: BTreeMap<(String, String), ConflictRecord>,
}

#[derive(Default)]
pub struct MemGraph {
    s: Mutex<GraphState>,
}

impl MemGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything — what losing the graph database looks like, for the
    /// tests that rebuild it from the oplog.
    pub fn wipe(&self) {
        *self.s.lock().unwrap() = GraphState::default();
    }
}

fn k(ws: &str, id: &str) -> (String, String) {
    (ws.to_string(), id.to_string())
}

impl Graph for MemGraph {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        let rec = s.symbols.entry(k(ws, key)).or_insert_with(|| SymbolRecord {
            key: key.to_string(),
            component: id.component.clone(),
            path: id.path.clone(),
            kind: id.kind,
            name: id.name.clone(),
            aliases: vec![],
            tip: None,
            deleted: true,
            position: None,
            wit_binding: None,
            last_op: 0,
        });
        if !rec.aliases.contains(&id.name) {
            rec.aliases.push(id.name.clone());
        }
        Ok(())
    }

    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if let Some(rec) = s.symbols.get_mut(&k(ws, key)) {
            if !rec.aliases.contains(&u.name) {
                rec.aliases.push(u.name.clone());
            }
            if rec.position.is_none() {
                rec.position = u.position_if_unset;
            }
            if u.op >= rec.last_op {
                rec.name = u.name;
                rec.tip = u.tip;
                rec.deleted = u.deleted;
                rec.wit_binding = u.wit_binding;
                rec.last_op = u.op;
            }
        }
        Ok(())
    }

    async fn symbol(&self, ws: &str, key: &str) -> Result<Option<SymbolRecord>> {
        Ok(self.s.lock().unwrap().symbols.get(&k(ws, key)).cloned())
    }

    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> Result<Vec<String>> {
        let s = self.s.lock().unwrap();
        Ok(s.symbols
            .iter()
            .filter(|((w, _), r)| {
                w == ws
                    && r.component == id.component
                    && r.path == id.path
                    && r.kind == id.kind
                    && r.aliases.contains(&id.name)
            })
            .map(|(_, r)| r.key.clone())
            .collect())
    }

    async fn symbols_in_component(&self, ws: &str, component: &str) -> Result<Vec<SymbolRecord>> {
        let s = self.s.lock().unwrap();
        Ok(s.symbols
            .iter()
            .filter(|((w, _), r)| w == ws && r.component == component)
            .map(|(_, r)| r.clone())
            .collect())
    }

    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        let slot = s.patches.entry(k(ws, &p.hash));
        match slot {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(p.clone());
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if o.get().status == PatchStatus::Pending {
                    o.insert(p.clone());
                }
            }
        }
        Ok(())
    }

    async fn patch(&self, ws: &str, hash: &str) -> Result<Option<PatchRecord>> {
        Ok(self.s.lock().unwrap().patches.get(&k(ws, hash)).cloned())
    }

    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if let Some(p) = s.patches.get_mut(&k(ws, hash)) {
            if op >= p.status_op {
                p.status = status;
                p.status_op = op;
                if set_op {
                    p.op = Some(op);
                }
            }
        }
        Ok(())
    }

    async fn dependents(&self, ws: &str, target: &SymbolId) -> Result<Vec<Hash>> {
        let s = self.s.lock().unwrap();
        let mut out: Vec<Hash> = s
            .patches
            .iter()
            .filter(|((w, _), p)| w == ws && p.depends_on.contains(target))
            .map(|(_, p)| p.hash.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        let fresh = |state_op: OpId| ConflictRecord {
            state: ConflictState::Open,
            opened_at: 0,
            resolved_by: None,
            pending: vec![op],
            state_op,
            ..c.clone()
        };
        match s.conflicts.get_mut(&k(ws, &c.id)) {
            None => {
                s.conflicts.insert(k(ws, &c.id), fresh(0));
            }
            Some(cur) if cur.state == ConflictState::Open && cur.opened_at == 0 => {
                if !cur.pending.contains(&op) {
                    cur.pending.push(op);
                }
            }
            Some(cur) if cur.state == ConflictState::Abandoned && op > cur.state_op => {
                *cur = fresh(cur.state_op);
            }
            Some(_) => {}
        }
        Ok(())
    }

    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if let Some(c) = s.conflicts.get_mut(&k(ws, id)) {
            c.pending.retain(|p| *p != op);
            if c.opened_at == 0 && c.state == ConflictState::Open && c.pending.is_empty() {
                c.state = ConflictState::Abandoned;
            }
        }
        Ok(())
    }

    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if let Some(c) = s.conflicts.get_mut(&k(ws, &e.conflict)) {
            if op >= c.state_op {
                if e.after == ConflictState::Open && c.opened_at == 0 {
                    c.opened_at = op;
                }
                c.state = e.after;
                c.resolved_by = e.resolved_by_after.clone();
                c.state_op = op;
                c.pending.retain(|p| *p != op);
            }
        }
        Ok(())
    }

    async fn conflict(&self, ws: &str, id: &str) -> Result<Option<ConflictRecord>> {
        Ok(self.s.lock().unwrap().conflicts.get(&k(ws, id)).cloned())
    }

    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> Result<Vec<ConflictRecord>> {
        let s = self.s.lock().unwrap();
        Ok(s.conflicts
            .iter()
            .filter(|((w, _), c)| w == ws && state.is_none_or(|st| c.state == st))
            .map(|(_, c)| c.clone())
            .collect())
    }

    async fn open_conflicts_for(&self, ws: &str, key: &str) -> Result<Vec<ConflictRecord>> {
        let s = self.s.lock().unwrap();
        Ok(s.conflicts
            .iter()
            .filter(|((w, _), c)| w == ws && c.key == key && c.state == ConflictState::Open)
            .map(|(_, c)| c.clone())
            .collect())
    }
}
