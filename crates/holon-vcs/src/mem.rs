//! No await is ever held under a lock, so a std `Mutex` is fine on any executor (and wasm32-wasip2).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Result;
use crate::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use crate::kvgraph::GraphState;
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

/// One revision counter across all keys, like a JetStream stream sequence, so a revision is never reused.
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
pub struct MemGraph {
    s: Mutex<HashMap<String, GraphState>>,
}

impl MemGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulates losing the graph database, for tests that rebuild it from the oplog.
    pub fn wipe(&self) {
        self.s.lock().unwrap().clear();
    }

    fn with<R>(&self, ws: &str, f: impl FnOnce(&mut GraphState) -> R) -> Result<R> {
        Ok(f(self.s.lock().unwrap().entry(ws.to_string()).or_default()))
    }
}

impl Graph for MemGraph {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> Result<()> {
        self.with(ws, |s| s.ensure_symbol(key, id))
    }
    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> Result<()> {
        self.with(ws, |s| s.update_symbol(key, u))
    }
    async fn symbol(&self, ws: &str, key: &str) -> Result<Option<SymbolRecord>> {
        self.with(ws, |s| s.symbol(key))
    }
    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> Result<Vec<String>> {
        self.with(ws, |s| s.symbols_named(id))
    }
    async fn symbols_in_component(&self, ws: &str, component: &str) -> Result<Vec<SymbolRecord>> {
        self.with(ws, |s| s.symbols_in_component(component))
    }
    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> Result<()> {
        self.with(ws, |s| s.put_patch(p))
    }
    async fn patch(&self, ws: &str, hash: &str) -> Result<Option<PatchRecord>> {
        self.with(ws, |s| s.patch(hash))
    }
    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> Result<()> {
        self.with(ws, |s| s.mark_patch(hash, status, op, set_op))
    }
    async fn dependents(&self, ws: &str, target: &SymbolId) -> Result<Vec<Hash>> {
        self.with(ws, |s| s.dependents(target))
    }
    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> Result<()> {
        self.with(ws, |s| s.put_conflict(c, op))
    }
    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> Result<()> {
        self.with(ws, |s| s.abandon_if_uncommitted(id, op))
    }
    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> Result<()> {
        self.with(ws, |s| s.apply_conflict_effect(e, op))
    }
    async fn conflict(&self, ws: &str, id: &str) -> Result<Option<ConflictRecord>> {
        self.with(ws, |s| s.conflict(id))
    }
    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> Result<Vec<ConflictRecord>> {
        self.with(ws, |s| s.conflicts(state))
    }
    async fn open_conflicts_for(&self, ws: &str, key: &str) -> Result<Vec<ConflictRecord>> {
        self.with(ws, |s| s.open_conflicts_for(key))
    }
}
