//! The graph over a [`Kv`]: a record per symbol, patch and conflict, each moved by compare-and-set,
//! and index lists that only grow. An index may name more than matches (it is written before its
//! record, and a patch's edges can be replaced), so every lookup reads the records and filters.
//! The rules for one record are shared with [`crate::mem::MemGraph`] through [`GraphState`].

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Result, VcsError};
use crate::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use crate::model::{ConflictState, Hash, OpId, SymbolId};
use crate::store::{escape, sha256_hex, Kv};

/// Racers on one record or index retry this often; there is no sleep in a guest to back off with.
const CAS_TRIES: u32 = 256;

fn new_symbol(key: &str, id: &SymbolId) -> SymbolRecord {
    SymbolRecord {
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
    }
}

fn add_alias(rec: &mut SymbolRecord, name: &str) {
    if !rec.aliases.iter().any(|a| a == name) {
        rec.aliases.push(name.to_string());
    }
}

fn update_symbol(rec: &mut SymbolRecord, u: &SymbolUpdate) {
    add_alias(rec, &u.name);
    if rec.position.is_none() {
        rec.position = u.position_if_unset;
    }
    if u.op >= rec.last_op {
        rec.name = u.name.clone();
        rec.tip = u.tip.clone();
        rec.deleted = u.deleted;
        rec.wit_binding = u.wit_binding.clone();
        rec.last_op = u.op;
    }
}

/// Replaces an existing record only while it is pending.
fn put_patch(cur: Option<&PatchRecord>, p: &PatchRecord) -> Option<PatchRecord> {
    match cur {
        Some(c) if c.status != PatchStatus::Pending => None,
        _ => Some(p.clone()),
    }
}

fn mark_patch(p: &mut PatchRecord, status: &PatchStatus, op: OpId, set_op: bool) {
    if op >= p.status_op {
        p.status = status.clone();
        p.status_op = op;
        if set_op {
            p.op = Some(op);
        }
    }
}

fn put_conflict(
    cur: Option<&ConflictRecord>,
    c: &ConflictRecord,
    op: OpId,
) -> Option<ConflictRecord> {
    let fresh = |state_op: OpId| ConflictRecord {
        state: ConflictState::Open,
        opened_at: 0,
        resolved_by: None,
        pending: vec![op],
        state_op,
        ..c.clone()
    };
    match cur {
        None => Some(fresh(0)),
        Some(cur) if cur.state == ConflictState::Open && cur.opened_at == 0 => {
            let mut next = cur.clone();
            if !next.pending.contains(&op) {
                next.pending.push(op);
            }
            Some(next)
        }
        Some(cur) if cur.state == ConflictState::Abandoned && op > cur.state_op => {
            Some(fresh(cur.state_op))
        }
        Some(_) => None,
    }
}

fn abandon_if_uncommitted(c: &mut ConflictRecord, op: OpId) {
    c.pending.retain(|p| *p != op);
    if c.opened_at == 0 && c.state == ConflictState::Open && c.pending.is_empty() {
        c.state = ConflictState::Abandoned;
    }
}

fn apply_conflict_effect(c: &mut ConflictRecord, e: &ConflictEffect, op: OpId) {
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

fn named(r: &SymbolRecord, id: &SymbolId) -> bool {
    r.component == id.component
        && r.path == id.path
        && r.kind == id.kind
        && r.aliases.contains(&id.name)
}

fn in_state(c: &ConflictRecord, state: Option<ConflictState>) -> bool {
    state.is_none_or(|st| c.state == st)
}

/// One workspace's graph in memory, for [`crate::mem::MemGraph`].
#[derive(Default)]
pub struct GraphState {
    symbols: BTreeMap<String, SymbolRecord>,
    patches: BTreeMap<Hash, PatchRecord>,
    conflicts: BTreeMap<String, ConflictRecord>,
}

impl GraphState {
    pub fn ensure_symbol(&mut self, key: &str, id: &SymbolId) {
        let rec = self.symbols.entry(key.to_string()).or_insert_with(|| new_symbol(key, id));
        add_alias(rec, &id.name);
    }
    pub fn update_symbol(&mut self, key: &str, u: SymbolUpdate) {
        if let Some(rec) = self.symbols.get_mut(key) {
            update_symbol(rec, &u);
        }
    }
    pub fn symbol(&self, key: &str) -> Option<SymbolRecord> {
        self.symbols.get(key).cloned()
    }
    pub fn symbols_named(&self, id: &SymbolId) -> Vec<String> {
        self.symbols.values().filter(|r| named(r, id)).map(|r| r.key.clone()).collect()
    }
    pub fn symbols_in_component(&self, component: &str) -> Vec<SymbolRecord> {
        self.symbols.values().filter(|r| r.component == component).cloned().collect()
    }
    pub fn put_patch(&mut self, p: &PatchRecord) {
        if let Some(next) = put_patch(self.patches.get(&p.hash), p) {
            self.patches.insert(p.hash.clone(), next);
        }
    }
    pub fn patch(&self, hash: &str) -> Option<PatchRecord> {
        self.patches.get(hash).cloned()
    }
    pub fn mark_patch(&mut self, hash: &str, status: PatchStatus, op: OpId, set_op: bool) {
        if let Some(p) = self.patches.get_mut(hash) {
            mark_patch(p, &status, op, set_op);
        }
    }
    pub fn dependents(&self, target: &SymbolId) -> Vec<Hash> {
        self.patches
            .values()
            .filter(|p| p.depends_on.contains(target))
            .map(|p| p.hash.clone())
            .collect()
    }
    pub fn put_conflict(&mut self, c: &ConflictRecord, op: OpId) {
        if let Some(next) = put_conflict(self.conflicts.get(&c.id), c, op) {
            self.conflicts.insert(c.id.clone(), next);
        }
    }
    pub fn abandon_if_uncommitted(&mut self, id: &str, op: OpId) {
        if let Some(c) = self.conflicts.get_mut(id) {
            abandon_if_uncommitted(c, op);
        }
    }
    pub fn apply_conflict_effect(&mut self, e: &ConflictEffect, op: OpId) {
        if let Some(c) = self.conflicts.get_mut(&e.conflict) {
            apply_conflict_effect(c, e, op);
        }
    }
    pub fn conflict(&self, id: &str) -> Option<ConflictRecord> {
        self.conflicts.get(id).cloned()
    }
    pub fn conflicts(&self, state: Option<ConflictState>) -> Vec<ConflictRecord> {
        self.conflicts.values().filter(|c| in_state(c, state)).cloned().collect()
    }
    pub fn open_conflicts_for(&self, key: &str) -> Vec<ConflictRecord> {
        self.conflicts
            .values()
            .filter(|c| c.key == key && c.state == ConflictState::Open)
            .cloned()
            .collect()
    }
}

pub struct KvGraph<K> {
    kv: K,
}

fn digest(parts: &[&str]) -> String {
    sha256_hex(parts.join("\n").as_bytes())
}

fn id_digest(id: &SymbolId) -> String {
    let kind = serde_json::to_string(&id.kind).unwrap_or_default();
    digest(&[&id.component, &id.path, &kind, &id.name])
}

impl<K: Kv> KvGraph<K> {
    pub fn new(kv: K) -> Self {
        Self { kv }
    }

    fn at(ws: &str, what: &str, d: &str) -> String {
        format!("graph/{}/{what}/{d}", escape(ws))
    }
    fn sym(ws: &str, key: &str) -> String {
        Self::at(ws, "sym", &digest(&[key]))
    }
    fn patch_key(ws: &str, hash: &str) -> String {
        Self::at(ws, "patch", &digest(&[hash]))
    }
    fn conflict_key(ws: &str, id: &str) -> String {
        Self::at(ws, "conflict", &digest(&[id]))
    }

    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.kv.get(key).await? {
            Some((b, _)) => Ok(Some(serde_json::from_slice(&b).map_err(VcsError::storage)?)),
            None => Ok(None),
        }
    }

    /// Read, change, compare-and-set, until it lands. `f` answering `None` writes nothing.
    async fn update<T: Serialize + DeserializeOwned>(
        &self,
        key: &str,
        f: impl Fn(Option<T>) -> Option<T>,
    ) -> Result<()> {
        for _ in 0..CAS_TRIES {
            let (cur, rev) = match self.kv.get(key).await? {
                Some((b, r)) => {
                    (Some(serde_json::from_slice(&b).map_err(VcsError::storage)?), Some(r))
                }
                None => (None, None),
            };
            let Some(next) = f(cur) else { return Ok(()) };
            let bytes = serde_json::to_vec(&next).map_err(VcsError::storage)?;
            if self.kv.cas(key, rev, bytes).await?.is_ok() {
                return Ok(());
            }
        }
        Err(VcsError::Storage(format!("{key} lost {CAS_TRIES} races")))
    }

    async fn index(&self, key: &str, item: &str) -> Result<()> {
        self.update::<Vec<String>>(key, |cur| {
            let mut list = cur.unwrap_or_default();
            (!list.iter().any(|i| i == item)).then(|| {
                list.push(item.to_string());
                list
            })
        })
        .await
    }

    async fn listed(&self, key: &str) -> Result<Vec<String>> {
        Ok(self.get::<Vec<String>>(key).await?.unwrap_or_default())
    }

    async fn conflicts_listed(&self, ws: &str, index: &str) -> Result<Vec<ConflictRecord>> {
        let mut out = Vec::new();
        for id in self.listed(index).await? {
            out.extend(self.get::<ConflictRecord>(&Self::conflict_key(ws, &id)).await?);
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

impl<K: Kv> Graph for KvGraph<K> {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> Result<()> {
        self.index(&Self::at(ws, "component", &digest(&[&id.component])), key).await?;
        self.index(&Self::at(ws, "named", &id_digest(id)), key).await?;
        self.update::<SymbolRecord>(&Self::sym(ws, key), |cur| {
            let mut rec = cur.unwrap_or_else(|| new_symbol(key, id));
            add_alias(&mut rec, &id.name);
            Some(rec)
        })
        .await
    }

    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> Result<()> {
        let Some(rec) = self.get::<SymbolRecord>(&Self::sym(ws, key)).await? else {
            return Ok(());
        };
        let id = SymbolId::new(&rec.component, &rec.path, &u.name, rec.kind);
        self.index(&Self::at(ws, "named", &id_digest(&id)), key).await?;
        self.update::<SymbolRecord>(&Self::sym(ws, key), |cur| {
            cur.map(|mut rec| {
                update_symbol(&mut rec, &u);
                rec
            })
        })
        .await
    }

    async fn symbol(&self, ws: &str, key: &str) -> Result<Option<SymbolRecord>> {
        self.get(&Self::sym(ws, key)).await
    }

    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for key in self.listed(&Self::at(ws, "named", &id_digest(id))).await? {
            if self.symbol(ws, &key).await?.is_some_and(|r| named(&r, id)) {
                out.push(key);
            }
        }
        out.sort();
        Ok(out)
    }

    async fn symbols_in_component(&self, ws: &str, component: &str) -> Result<Vec<SymbolRecord>> {
        let mut out = Vec::new();
        for key in self.listed(&Self::at(ws, "component", &digest(&[component]))).await? {
            out.extend(self.symbol(ws, &key).await?.filter(|r| r.component == component));
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> Result<()> {
        for target in &p.depends_on {
            self.index(&Self::at(ws, "dependents", &id_digest(target)), &p.hash).await?;
        }
        self.update::<PatchRecord>(&Self::patch_key(ws, &p.hash), |cur| put_patch(cur.as_ref(), p))
            .await
    }

    async fn patch(&self, ws: &str, hash: &str) -> Result<Option<PatchRecord>> {
        self.get(&Self::patch_key(ws, hash)).await
    }

    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> Result<()> {
        self.update::<PatchRecord>(&Self::patch_key(ws, hash), |cur| {
            cur.map(|mut p| {
                mark_patch(&mut p, &status, op, set_op);
                p
            })
        })
        .await
    }

    async fn dependents(&self, ws: &str, target: &SymbolId) -> Result<Vec<Hash>> {
        let mut out = Vec::new();
        for hash in self.listed(&Self::at(ws, "dependents", &id_digest(target))).await? {
            if self.patch(ws, &hash).await?.is_some_and(|p| p.depends_on.contains(target)) {
                out.push(hash);
            }
        }
        out.sort();
        Ok(out)
    }

    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> Result<()> {
        self.index(&Self::at(ws, "conflicts", "all"), &c.id).await?;
        self.index(&Self::at(ws, "key-conflicts", &digest(&[&c.key])), &c.id).await?;
        self.update::<ConflictRecord>(&Self::conflict_key(ws, &c.id), |cur| {
            put_conflict(cur.as_ref(), c, op)
        })
        .await
    }

    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> Result<()> {
        self.update::<ConflictRecord>(&Self::conflict_key(ws, id), |cur| {
            cur.map(|mut c| {
                abandon_if_uncommitted(&mut c, op);
                c
            })
        })
        .await
    }

    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> Result<()> {
        self.update::<ConflictRecord>(&Self::conflict_key(ws, &e.conflict), |cur| {
            cur.map(|mut c| {
                apply_conflict_effect(&mut c, e, op);
                c
            })
        })
        .await
    }

    async fn conflict(&self, ws: &str, id: &str) -> Result<Option<ConflictRecord>> {
        self.get(&Self::conflict_key(ws, id)).await
    }

    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> Result<Vec<ConflictRecord>> {
        let all = self.conflicts_listed(ws, &Self::at(ws, "conflicts", "all")).await?;
        Ok(all.into_iter().filter(|c| in_state(c, state)).collect())
    }

    async fn open_conflicts_for(&self, ws: &str, key: &str) -> Result<Vec<ConflictRecord>> {
        let index = Self::at(ws, "key-conflicts", &digest(&[key]));
        let all = self.conflicts_listed(ws, &index).await?;
        Ok(all.into_iter().filter(|c| c.key == key && c.state == ConflictState::Open).collect())
    }
}
