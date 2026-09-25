//! The structural graph: `symbol`, `patch`, `conflict`, and the `depends_on` /
//! `implements` / `conflicts_with` edges — as a trait, implemented in memory
//! ([`crate::mem::MemGraph`]) and on SurrealDB (`surreal::SurrealGraph`).
//!
//! # What is authoritative
//!
//! The graph is an INDEX. What a symbol *is* right now is its tip pointer in the
//! [`crate::store::PointerStore`]; the graph holds the immutable patch records
//! those pointers name, the conflict records, and mirrors (a symbol's current
//! name, tip, position) that make listing a component possible without scanning
//! pointers. The engine always re-reads the pointer and verifies against the
//! patch it names, so a mirror that lags a pointer can make a lookup miss for a
//! moment, never make a write land on the wrong state.
//!
//! Every record is per workspace: the same patch hash in two workspaces is two
//! records (its landing op and status differ).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{Agent, ConflictId, ConflictState, Hash, OpId, SymbolId, SymbolKind};

/// A patch's change, normalised: content is always a blob hash, never inline.
/// This (with the symbol key and the parents) is what a patch hash covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Change {
    Create(Hash),
    Replace(Hash),
    Delete,
    Rename(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status", content = "conflict")]
pub enum PatchStatus {
    /// Written before the compare-and-set that would land it; not (yet) anything.
    Pending,
    /// Was a symbol's tip as of `PatchRecord::op` (a later revert does not change
    /// this — the oplog says what is live).
    Landed,
    /// Recorded as the right side of this conflict.
    Conflicted(ConflictId),
}

/// An immutable patch, plus the metadata that is not part of its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchRecord {
    pub hash: Hash,
    /// The stable symbol key (see [`crate::patch::symbol_key`]).
    pub key: String,
    /// The symbol's identity AFTER this patch (a rename's new name).
    pub symbol: SymbolId,
    pub parents: Vec<Hash>,
    pub change: Change,
    /// The symbol's content after this patch; `None` for a delete.
    pub content: Option<Hash>,
    pub agent: Agent,
    pub message: Option<String>,
    /// Unix ms the engine recorded it.
    pub at: u64,
    /// The op that landed it (or opened its conflict).
    pub op: Option<OpId>,
    pub status: PatchStatus,
    pub depends_on: Vec<SymbolId>,
    pub implements: Vec<SymbolId>,
    pub wit_binding: Option<String>,
}

impl PatchRecord {
    pub fn is_delete(&self) -> bool {
        self.content.is_none()
    }
}

/// A symbol's index entry. `component`, `path` and `kind` never change; `name`
/// and the rest mirror the tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub key: String,
    pub component: String,
    pub path: String,
    pub kind: SymbolKind,
    pub name: String,
    /// Every name this symbol has been written under — what a lookup by name
    /// searches, so a rename is findable before its mirror update lands.
    pub aliases: Vec<String>,
    pub tip: Option<Hash>,
    pub deleted: bool,
    /// Order within its file: the op that first created it. `None` until that op
    /// is in the log.
    pub position: Option<OpId>,
    pub wit_binding: Option<String>,
}

/// The mirror fields an op updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolUpdate {
    pub name: String,
    pub tip: Option<Hash>,
    pub deleted: bool,
    pub wit_binding: Option<String>,
    /// Set as the position only if the symbol has none yet.
    pub position_if_unset: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideRecord {
    pub patch: Hash,
    pub agent: Agent,
    /// Content blob; `None` when this side deleted the symbol.
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
    /// 0 until the op that opened it is in the log (see the engine's notes on
    /// uncommitted conflicts).
    pub opened_at: OpId,
    pub resolved_by: Option<Hash>,
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

/// The graph store.
pub trait Graph: Send + Sync {
    // ---- symbols -------------------------------------------------------------

    /// Create the symbol's index entry if absent (tip `none`, deleted, no
    /// position), and add `name` to its aliases either way.
    fn ensure_symbol(
        &self,
        ws: &str,
        key: &str,
        id: &SymbolId,
    ) -> impl Future<Output = Result<()>> + Send;
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
    /// Keys of symbols with this component, path and kind that have EVER been
    /// called `name` (candidates; the caller verifies against the tip).
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

    // ---- patches -------------------------------------------------------------

    /// Insert a patch record; if one with this hash exists it is replaced only
    /// while it is still `Pending`. Replaces its `depends_on`/`implements` edges.
    fn put_patch(&self, ws: &str, patch: &PatchRecord) -> impl Future<Output = Result<()>> + Send;
    fn patch(
        &self,
        ws: &str,
        hash: &str,
    ) -> impl Future<Output = Result<Option<PatchRecord>>> + Send;
    /// Set a patch's status, and its op when `op` is `Some`.
    fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: Option<OpId>,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Hashes of patches with a `depends_on` edge to `target`.
    fn dependents(
        &self,
        ws: &str,
        target: &SymbolId,
    ) -> impl Future<Output = Result<Vec<Hash>>> + Send;

    // ---- conflicts -----------------------------------------------------------

    /// Insert or replace a conflict record (and its `conflicts_with` edge).
    fn put_conflict(&self, ws: &str, c: &ConflictRecord)
        -> impl Future<Output = Result<()>> + Send;
    /// Atomically: `abandoned` if and only if `opened_at` is still 0.
    fn abandon_if_uncommitted(&self, ws: &str, id: &str)
        -> impl Future<Output = Result<()>> + Send;
    /// `open`, opened at `op`.
    fn commit_conflict(
        &self,
        ws: &str,
        id: &str,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send;
    fn set_conflict_state(
        &self,
        ws: &str,
        id: &str,
        state: ConflictState,
        resolved_by: Option<Hash>,
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

    // ---- op side effects -----------------------------------------------------

    fn put_effects(
        &self,
        ws: &str,
        op: OpId,
        effects: &[ConflictEffect],
    ) -> impl Future<Output = Result<()>> + Send;
    fn effects(
        &self,
        ws: &str,
        op: OpId,
    ) -> impl Future<Output = Result<Vec<ConflictEffect>>> + Send;
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
        op: Option<OpId>,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).mark_patch(ws, hash, status, op)
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
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).put_conflict(ws, c)
    }
    fn abandon_if_uncommitted(
        &self,
        ws: &str,
        id: &str,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).abandon_if_uncommitted(ws, id)
    }
    fn commit_conflict(
        &self,
        ws: &str,
        id: &str,
        op: OpId,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).commit_conflict(ws, id, op)
    }
    fn set_conflict_state(
        &self,
        ws: &str,
        id: &str,
        state: ConflictState,
        resolved_by: Option<Hash>,
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).set_conflict_state(ws, id, state, resolved_by)
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
    fn put_effects(
        &self,
        ws: &str,
        op: OpId,
        effects: &[ConflictEffect],
    ) -> impl Future<Output = Result<()>> + Send {
        (**self).put_effects(ws, op, effects)
    }
    fn effects(
        &self,
        ws: &str,
        op: OpId,
    ) -> impl Future<Output = Result<Vec<ConflictEffect>>> + Send {
        (**self).effects(ws, op)
    }
}
