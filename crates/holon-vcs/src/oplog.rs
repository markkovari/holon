//! The linear, undoable operation log: every state mutation, with the pointer
//! moves needed to revert it.
//!
//! [`KvOpLog`] is the one implementation, over any [`Kv`] — in memory for tests
//! and wasm, JetStream KV natively. Per workspace `w` (escaped, see
//! [`crate::store::escape`]):
//!
//! * `oplog/<w>/op/<id:020>` — the entry, JSON ([`StoredOp`]). Created with
//!   `create` (expected: absent) in state `pending`, then rewritten exactly once,
//!   by CAS, to `committed` or `aborted` ([`OpLog::finish`]); never deleted.
//! * `oplog/<w>/head` — the highest id known to be written, as decimal.
//! * `oplog/<w>/settled` — an id at or below which every op is known to be
//!   committed or aborted (a hint that only moves forward; readers walk past it).
//!
//! # Intents
//!
//! An op is appended BEFORE the pointer writes it describes (write-ahead): the
//! entry carries, besides the contract's [`OpEntry`], everything needed to
//! finish or undo it from the log alone — the revision and op each pointer
//! write expects ([`Guard`]), and the patch records, conflict records and
//! conflict effects it introduces ([`Intent`]). What `pending` means, and who
//! may move it on, is [`crate::recovery`]'s business; this module only stores.
//!
//! # Append
//!
//! Read the head `h`; walk forward past any entries that exist beyond it (an
//! appender that died between writing its entry and advancing the head leaves the
//! head one behind); `create` the first missing id. Exactly one appender wins a
//! given id — the others see the key exist and go round again. The winner then
//! advances the head by CAS, only ever forwards, and helps any lag it finds.
//!
//! Entries are dense: id `n+1` is only created after id `n` was seen to exist, so
//! a reader walking from `after+1` until the first missing id sees a prefix of the
//! log with no holes.

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::{Result, VcsError};
use crate::graph::{ConflictEffect, ConflictRecord, PatchRecord};
use crate::model::{Agent, OpEntry, OpId, OpKind, PointerMove};
use crate::store::{escape, Kv, Revision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpState {
    /// Logged; its pointer writes may or may not have happened.
    Pending,
    /// Its primary pointer write happened and everything after it is done.
    Committed,
    /// Its primary pointer write did not happen and now never can.
    Aborted,
}

/// What one move's compare-and-set expects: the revision read (`None`: the key
/// did not exist) and the op the value read named — so the value a pointer held
/// before this op can be put back exactly, and so the ops that wrote a pointer
/// form a chain back through time (see [`crate::recovery`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub rev: Option<Revision>,
    pub op: Option<OpId>,
}

/// What an op writes besides pointers — logged so that the graph can be
/// finished, or rebuilt, from the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// One per move, in order.
    pub guards: Vec<Guard>,
    /// Patch records this op introduces.
    pub patches: Vec<PatchRecord>,
    /// Conflict records this op writes ahead (uncommitted).
    pub conflicts: Vec<ConflictRecord>,
    /// Conflict state changes it makes when it commits (and a revert inverts).
    pub effects: Vec<ConflictEffect>,
}

/// An entry as the log keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOp {
    #[serde(flatten)]
    pub entry: OpEntry,
    pub state: OpState,
    #[serde(default)]
    pub intent: Intent,
}

impl StoredOp {
    pub fn id(&self) -> OpId {
        self.entry.id
    }
    /// The guard of the move on `pointer`.
    pub fn guard(&self, pointer: &str) -> Option<&Guard> {
        let i = self.entry.moves.iter().position(|m| m.pointer == pointer)?;
        self.intent.guards.get(i)
    }
}

/// An entry before it has an id.
#[derive(Debug, Clone)]
pub struct NewOp {
    pub at: u64,
    pub agent: Agent,
    pub kind: OpKind,
    pub moves: Vec<PointerMove>,
    pub intent: Intent,
}

pub trait OpLog: Send + Sync {
    /// Append in state `pending`, assigning the next id.
    fn append(&self, ws: &str, op: NewOp) -> impl Future<Output = Result<StoredOp>> + Send;
    fn get(&self, ws: &str, id: OpId) -> impl Future<Output = Result<Option<StoredOp>>> + Send;
    /// Move a pending op to `state` (`committed` or `aborted`), once: returns
    /// the state it ends in, which is an earlier finisher's if there was one.
    fn finish(
        &self,
        ws: &str,
        id: OpId,
        state: OpState,
    ) -> impl Future<Output = Result<OpState>> + Send;
    /// Oldest first, ids strictly greater than `after`, at most `limit`, in any
    /// state.
    fn list(
        &self,
        ws: &str,
        after: Option<OpId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<StoredOp>>> + Send;
    /// The newest id, `None` for an empty log.
    fn latest(&self, ws: &str) -> impl Future<Output = Result<Option<OpId>>> + Send;
    /// The settled hint (0 when unset).
    fn settled(&self, ws: &str) -> impl Future<Output = Result<OpId>> + Send;
    /// Raise the settled hint to at least `to`; never lowers it.
    fn advance_settled(&self, ws: &str, to: OpId) -> impl Future<Output = Result<()>> + Send;
}

/// How many times one append may lose the race for an id. Every loss is another
/// appender's success, so this bounds starvation, not livelock (see #284).
pub const APPEND_TRIES: u32 = 1000;

pub struct KvOpLog<K> {
    kv: K,
}

impl<K> KvOpLog<K> {
    pub fn new(kv: K) -> Self {
        KvOpLog { kv }
    }
    pub fn kv(&self) -> &K {
        &self.kv
    }
}

/// The workspace an oplog key's head names: `oplog/<escaped ws>/head` → the
/// workspace. `None` for any other key (entries, the settled hint, a hashed
/// over-long key). What a daemon lists to know which workspaces to repair.
pub fn workspace_of_head_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix("oplog/")?.strip_suffix("/head")?;
    if rest.contains('/') {
        return None;
    }
    crate::store::unescape(rest)
}

fn head_key(ws: &str) -> String {
    format!("oplog/{}/head", escape(ws))
}

fn entry_key(ws: &str, id: OpId) -> String {
    format!("oplog/{}/op/{id:020}", escape(ws))
}

fn settled_key(ws: &str) -> String {
    format!("oplog/{}/settled", escape(ws))
}

impl<K: Kv> KvOpLog<K> {
    async fn counter(&self, key: &str) -> Result<(OpId, Option<u64>)> {
        match self.kv.get(key).await? {
            None => Ok((0, None)),
            Some((bytes, rev)) => {
                let s = std::str::from_utf8(&bytes).map_err(VcsError::storage)?;
                let n =
                    s.parse::<OpId>().map_err(|_| VcsError::Storage(format!("{key} is {s:?}")))?;
                Ok((n, Some(rev)))
            }
        }
    }

    async fn head(&self, ws: &str) -> Result<(OpId, Option<u64>)> {
        self.counter(&head_key(ws)).await
    }

    /// Move a counter forwards to at least `target`; never backwards.
    async fn advance(&self, key: &str, target: OpId) -> Result<bool> {
        for _ in 0..APPEND_TRIES {
            let (h, rev) = self.counter(key).await?;
            if h >= target {
                return Ok(true);
            }
            let bytes = target.to_string().into_bytes();
            if self.kv.cas(key, rev, bytes).await?.is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The last id that exists, starting the walk at the head.
    async fn tail(&self, ws: &str) -> Result<OpId> {
        let (mut n, _) = self.head(ws).await?;
        while self.kv.get(&entry_key(ws, n + 1)).await?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    async fn raw(&self, ws: &str, id: OpId) -> Result<Option<(StoredOp, Revision)>> {
        match self.kv.get(&entry_key(ws, id)).await? {
            None => Ok(None),
            Some((bytes, rev)) => {
                serde_json::from_slice(&bytes).map(|op| Some((op, rev))).map_err(VcsError::storage)
            }
        }
    }
}

impl<K: Kv> OpLog for KvOpLog<K> {
    async fn append(&self, ws: &str, op: NewOp) -> Result<StoredOp> {
        for _ in 0..APPEND_TRIES {
            let id = self.tail(ws).await? + 1;
            let stored = StoredOp {
                entry: OpEntry {
                    id,
                    workspace: ws.to_string(),
                    at: op.at,
                    agent: op.agent.clone(),
                    kind: op.kind.clone(),
                    moves: op.moves.clone(),
                },
                state: OpState::Pending,
                intent: op.intent.clone(),
            };
            let bytes = serde_json::to_vec(&stored).map_err(VcsError::storage)?;
            match self.kv.cas(&entry_key(ws, id), None, bytes).await? {
                Ok(_) => {
                    // The entry is written and readers walk past a lagging head,
                    // so a head that will not move is not a lost op.
                    self.advance(&head_key(ws), id).await?;
                    return Ok(stored);
                }
                Err(_) => continue, // somebody else took `id`; go again
            }
        }
        Err(VcsError::Storage(format!("oplog append to {ws} lost {APPEND_TRIES} races")))
    }

    async fn get(&self, ws: &str, id: OpId) -> Result<Option<StoredOp>> {
        Ok(self.raw(ws, id).await?.map(|(op, _)| op))
    }

    async fn finish(&self, ws: &str, id: OpId, state: OpState) -> Result<OpState> {
        for _ in 0..APPEND_TRIES {
            let Some((mut op, rev)) = self.raw(ws, id).await? else {
                return Err(VcsError::NotFound(format!("op {id} in {ws}")));
            };
            if op.state != OpState::Pending {
                return Ok(op.state);
            }
            op.state = state;
            let bytes = serde_json::to_vec(&op).map_err(VcsError::storage)?;
            if self.kv.cas(&entry_key(ws, id), Some(rev), bytes).await?.is_ok() {
                return Ok(state);
            }
        }
        Err(VcsError::Storage(format!("op {id} in {ws} would not finish")))
    }

    async fn settled(&self, ws: &str) -> Result<OpId> {
        Ok(self.counter(&settled_key(ws)).await?.0)
    }

    async fn advance_settled(&self, ws: &str, to: OpId) -> Result<()> {
        // A hint: losing every race to raise it only means the next reader walks
        // a little further.
        self.advance(&settled_key(ws), to).await.map(|_| ())
    }

    async fn list(&self, ws: &str, after: Option<OpId>, limit: u32) -> Result<Vec<StoredOp>> {
        let mut out = Vec::new();
        let mut id = after.unwrap_or(0) + 1;
        while out.len() < limit as usize {
            match self.get(ws, id).await? {
                Some(e) => out.push(e),
                None => break,
            }
            id += 1;
        }
        Ok(out)
    }

    async fn latest(&self, ws: &str) -> Result<Option<OpId>> {
        let n = self.tail(ws).await?;
        Ok((n > 0).then_some(n))
    }
}

impl<T: OpLog> OpLog for std::sync::Arc<T> {
    fn append(&self, ws: &str, op: NewOp) -> impl Future<Output = Result<StoredOp>> + Send {
        (**self).append(ws, op)
    }
    fn get(&self, ws: &str, id: OpId) -> impl Future<Output = Result<Option<StoredOp>>> + Send {
        (**self).get(ws, id)
    }
    fn finish(
        &self,
        ws: &str,
        id: OpId,
        state: OpState,
    ) -> impl Future<Output = Result<OpState>> + Send {
        (**self).finish(ws, id, state)
    }
    fn list(
        &self,
        ws: &str,
        after: Option<OpId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<StoredOp>>> + Send {
        (**self).list(ws, after, limit)
    }
    fn latest(&self, ws: &str) -> impl Future<Output = Result<Option<OpId>>> + Send {
        (**self).latest(ws)
    }
    fn settled(&self, ws: &str) -> impl Future<Output = Result<OpId>> + Send {
        (**self).settled(ws)
    }
    fn advance_settled(&self, ws: &str, to: OpId) -> impl Future<Output = Result<()>> + Send {
        (**self).advance_settled(ws, to)
    }
}
