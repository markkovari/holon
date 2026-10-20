//! The write-ahead op log over a [`Kv`]. Entries are dense (id `n+1` is created only after `n`
//! exists), so a reader walking forward sees a hole-free prefix; the head may lag one behind.

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
    Committed,
    /// Its primary pointer write did not happen and now never can.
    Aborted,
}

/// `rev` `None`: the key did not exist. `op` lets the old value be put back exactly, and chains a
/// pointer's writers back through time (see [`crate::recovery`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub rev: Option<Revision>,
    pub op: Option<OpId>,
}

/// Logged so the graph can be finished, or rebuilt, from the log alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// One per move, in order.
    pub guards: Vec<Guard>,
    pub patches: Vec<PatchRecord>,
    pub conflicts: Vec<ConflictRecord>,
    /// Conflict state changes it makes when it commits (and a revert inverts).
    pub effects: Vec<ConflictEffect>,
}

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
    pub fn guard(&self, pointer: &str) -> Option<&Guard> {
        let i = self.entry.moves.iter().position(|m| m.pointer == pointer)?;
        self.intent.guards.get(i)
    }
}

#[derive(Debug, Clone)]
pub struct NewOp {
    pub at: u64,
    pub agent: Agent,
    pub kind: OpKind,
    pub moves: Vec<PointerMove>,
    pub intent: Intent,
}

pub trait OpLog: Send + Sync {
    fn append(&self, ws: &str, op: NewOp) -> impl Future<Output = Result<StoredOp>> + Send;
    fn get(&self, ws: &str, id: OpId) -> impl Future<Output = Result<Option<StoredOp>>> + Send;
    /// Once: returns the state it ends in, an earlier finisher's if there was one.
    fn finish(
        &self,
        ws: &str,
        id: OpId,
        state: OpState,
    ) -> impl Future<Output = Result<OpState>> + Send;
    /// Oldest first, ids after `after`, in any state.
    fn list(
        &self,
        ws: &str,
        after: Option<OpId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<StoredOp>>> + Send;
    fn latest(&self, ws: &str) -> impl Future<Output = Result<Option<OpId>>> + Send;
    fn settled(&self, ws: &str) -> impl Future<Output = Result<OpId>> + Send;
    fn advance_settled(&self, ws: &str, to: OpId) -> impl Future<Output = Result<()>> + Send;
}

/// Every lost race is another appender's success, so this bounds starvation, not livelock (#284).
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

/// `oplog/<escaped ws>/head` → the workspace; what a daemon lists to find workspaces to repair.
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
                    // Readers walk past a lagging head, so a head that will not move loses no op.
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
        // A hint: losing every race only means the next reader walks a little further.
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
