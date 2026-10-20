//! An index: a symbol's truth is its tip pointer, so a lagging mirror here can miss, never mislead.
//! Every mutable field records the op that set it; a write from an older op is ignored.

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{Agent, ConflictId, ConflictState, Hash, OpId, Placement, SymbolId, SymbolKind};

/// Normalised (content is a blob hash); with the key and parents, what a patch hash covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Change {
    Create(Hash),
    Replace(Hash),
    Delete,
    Rename(String),
    /// As requested (hashed); the resolved order key is [`PatchRecord::order`].
    Move(Placement),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status", content = "conflict")]
pub enum PatchStatus {
    /// Written before the compare-and-set that would land it; not (yet) anything.
    Pending,
    /// Was a tip as of `PatchRecord::op`; a later revert does not change this.
    Landed,
    /// Recorded as the right side of this conflict.
    Conflicted(ConflictId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchRecord {
    pub hash: Hash,
    pub key: String,
    /// The symbol's identity AFTER this patch (a rename's new name).
    pub symbol: SymbolId,
    pub parents: Vec<Hash>,
    pub change: Change,
    pub content: Option<Hash>,
    pub agent: Agent,
    pub message: Option<String>,
    /// Unix ms.
    pub at: u64,
    /// The op that landed it (or opened its conflict).
    pub op: Option<OpId>,
    pub status: PatchStatus,
    pub depends_on: Vec<SymbolId>,
    pub implements: Vec<SymbolId>,
    pub wit_binding: Option<String>,
    #[serde(default)]
    pub status_op: OpId,
    /// `None`: ordered by creation op; carried forward by patches that do not set it.
    #[serde(default)]
    pub order: Option<String>,
    /// The placement a `create` asked for (hashed, with the change).
    #[serde(default)]
    pub placement: Option<Placement>,
}

impl PatchRecord {
    pub fn is_delete(&self) -> bool {
        self.content.is_none()
    }
}

/// `component`, `path` and `kind` never change; the rest mirror the tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub key: String,
    pub component: String,
    pub path: String,
    pub kind: SymbolKind,
    pub name: String,
    /// Every name it has had, so a rename is findable before its mirror update lands.
    pub aliases: Vec<String>,
    pub tip: Option<Hash>,
    pub deleted: bool,
    /// The creating op, its file order absent an order key; `None` until that op finishes.
    pub position: Option<OpId>,
    pub wit_binding: Option<String>,
    #[serde(default)]
    pub last_op: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolUpdate {
    pub name: String,
    pub tip: Option<Hash>,
    pub deleted: bool,
    pub wit_binding: Option<String>,
    pub position_if_unset: Option<OpId>,
    pub op: OpId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideRecord {
    pub patch: Hash,
    pub agent: Agent,
    pub content: Option<Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub id: ConflictId,
    pub key: String,
    pub symbol: SymbolId,
    pub base: Option<Hash>,
    pub left: SideRecord,
    pub right: SideRecord,
    pub state: ConflictState,
    /// 0 until the opening op commits.
    pub opened_at: OpId,
    pub resolved_by: Option<Hash>,
    /// While uncommitted, the ops that wrote it; abandoned only when the last of them aborts.
    #[serde(default)]
    pub pending: Vec<OpId>,
    #[serde(default)]
    pub state_op: OpId,
}

/// One conflict state change an op made, so reverting the op can undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEffect {
    pub conflict: ConflictId,
    pub before: ConflictState,
    pub after: ConflictState,
    pub resolved_by_before: Option<Hash>,
    pub resolved_by_after: Option<Hash>,
}

impl ConflictEffect {
    pub fn inverse(&self) -> Self {
        ConflictEffect {
            conflict: self.conflict.clone(),
            before: self.after,
            after: self.before,
            resolved_by_before: self.resolved_by_after.clone(),
            resolved_by_after: self.resolved_by_before.clone(),
        }
    }
}

pub trait Graph: Send + Sync {
    /// Creates it if absent (no tip, deleted, no position); adds `name` to aliases either way.
    fn ensure_symbol(
        &self,
        ws: &str,
        key: &str,
        id: &SymbolId,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Applies only if `update.op` >= `last_op`; sets an unset position either way.
    fn update_symbol(
        &self,
        ws: &str,
        key: &str,
        update: SymbolUpdate,
    ) -> impl Future<Output = Result<()>> + Send;
    fn symbol(
        &self,
        ws: &str,
        key: &str,
    ) -> impl Future<Output = Result<Option<SymbolRecord>>> + Send;
    /// Keys ever called `name` (candidates; the caller verifies against the tip).
    fn symbols_named(
        &self,
        ws: &str,
        id: &SymbolId,
    ) -> impl Future<Output = Result<Vec<String>>> + Send;
    /// Every symbol ever indexed in a component, live or not.
    fn symbols_in_component(
        &self,
        ws: &str,
        component: &str,
    ) -> impl Future<Output = Result<Vec<SymbolRecord>>> + Send;

    /// Replaces an existing record only while it is `Pending`; replaces its edges.
    fn put_patch(&self, ws: &str, patch: &PatchRecord) -> impl Future<Output = Result<()>> + Send;
    fn patch(
        &self,
        ws: &str,
        hash: &str,
    ) -> impl Future<Output = Result<Option<PatchRecord>>> + Send;
    /// Only if `op` >= `status_op`; `set_op` also sets the patch's `op`.
    fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Hashes of patches with a `depends_on` edge to `target`.
    fn dependents(
        &self,
        ws: &str,
        target: &SymbolId,
    ) -> impl Future<Output = Result<Vec<Hash>>> + Send;

    /// Writes it uncommitted (`opened_at: 0`, `open`) ahead of `op`'s pointer write. If it exists:
    /// uncommitted and open, `op` joins `pending`; abandoned and older than `op`, rewritten; else kept.
    fn put_conflict(
        &self,
        ws: &str,
        c: &ConflictRecord,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Drops `op` from `pending`; abandons it if uncommitted, open, and nothing is left pending.
    fn abandon_if_uncommitted(
        &self,
        ws: &str,
        id: &str,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Only if `op` >= `state_op`; an effect to `open` on an uncommitted conflict commits it.
    fn apply_conflict_effect(
        &self,
        ws: &str,
        effect: &ConflictEffect,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send;
    fn conflict(
        &self,
        ws: &str,
        id: &str,
    ) -> impl Future<Output = Result<Option<ConflictRecord>>> + Send;
    fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> impl Future<Output = Result<Vec<ConflictRecord>>> + Send;
    fn open_conflicts_for(
        &self,
        ws: &str,
        key: &str,
    ) -> impl Future<Output = Result<Vec<ConflictRecord>>> + Send;
}

impl<T: Graph> Graph for std::sync::Arc<T> {
    fn ensure_symbol(
        &self,
        ws: &str,
        key: &str,
        id: &SymbolId,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).ensure_symbol(ws, key, id)
    }
    fn update_symbol(
        &self,
        ws: &str,
        key: &str,
        update: SymbolUpdate,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).update_symbol(ws, key, update)
    }
    fn symbol(
        &self,
        ws: &str,
        key: &str,
    ) -> impl Future<Output = Result<Option<SymbolRecord>>> + Send {
        (**self).symbol(ws, key)
    }
    fn symbols_named(
        &self,
        ws: &str,
        id: &SymbolId,
    ) -> impl Future<Output = Result<Vec<String>>> + Send {
        (**self).symbols_named(ws, id)
    }
    fn symbols_in_component(
        &self,
        ws: &str,
        component: &str,
    ) -> impl Future<Output = Result<Vec<SymbolRecord>>> + Send {
        (**self).symbols_in_component(ws, component)
    }
    fn put_patch(&self, ws: &str, patch: &PatchRecord) -> impl Future<Output = Result<()>> + Send {
        (**self).put_patch(ws, patch)
    }
    fn patch(
        &self,
        ws: &str,
        hash: &str,
    ) -> impl Future<Output = Result<Option<PatchRecord>>> + Send {
        (**self).patch(ws, hash)
    }
    fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).mark_patch(ws, hash, status, op, set_op)
    }
    fn dependents(
        &self,
        ws: &str,
        target: &SymbolId,
    ) -> impl Future<Output = Result<Vec<Hash>>> + Send {
        (**self).dependents(ws, target)
    }
    fn put_conflict(
        &self,
        ws: &str,
        c: &ConflictRecord,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).put_conflict(ws, c, op)
    }
    fn abandon_if_uncommitted(
        &self,
        ws: &str,
        id: &str,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).abandon_if_uncommitted(ws, id, op)
    }
    fn apply_conflict_effect(
        &self,
        ws: &str,
        effect: &ConflictEffect,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).apply_conflict_effect(ws, effect, op)
    }
    fn conflict(
        &self,
        ws: &str,
        id: &str,
    ) -> impl Future<Output = Result<Option<ConflictRecord>>> + Send {
        (**self).conflict(ws, id)
    }
    fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> impl Future<Output = Result<Vec<ConflictRecord>>> + Send {
        (**self).conflicts(ws, state)
    }
    fn open_conflicts_for(
        &self,
        ws: &str,
        key: &str,
    ) -> impl Future<Output = Result<Vec<ConflictRecord>>> + Send {
        (**self).open_conflicts_for(ws, key)
    }
}
