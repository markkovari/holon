//! The linear, undoable operation log: every state mutation, with the pointer
//! moves needed to revert it.
//!
//! [`KvOpLog`] is the one implementation, over any [`Kv`] — in memory for tests
//! and wasm, JetStream KV natively. Per workspace `w` (escaped, see
//! [`crate::store::escape`]):
//!
//! * `oplog/<w>/op/<id:020>` — the entry, JSON. Written once, with `create`
//!   (expected: absent), never rewritten, never deleted.
//! * `oplog/<w>/head` — the highest id known to be written, as decimal.
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

use crate::error::{Result, VcsError};
use crate::model::{Agent, OpEntry, OpId, OpKind, PointerMove};
use crate::store::{escape, Kv};

/// An entry before it has an id.
#[derive(Debug, Clone)]
pub struct NewOp {
    pub at: u64,
    pub agent: Agent,
    pub kind: OpKind,
    pub moves: Vec<PointerMove>,
}

pub trait OpLog: Send + Sync {
    /// Append, assigning the next id.
    fn append(&self, ws: &str, op: NewOp) -> impl Future<Output = Result<OpEntry>> + Send;
    fn get(&self, ws: &str, id: OpId) -> impl Future<Output = Result<Option<OpEntry>>> + Send;
    /// Oldest first, ids strictly greater than `after`, at most `limit`.
    fn list(
        &self,
        ws: &str,
        after: Option<OpId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<OpEntry>>> + Send;
    /// The newest id, `None` for an empty log.
    fn latest(&self, ws: &str) -> impl Future<Output = Result<Option<OpId>>> + Send;
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
}

fn head_key(ws: &str) -> String {
    format!("oplog/{}/head", escape(ws))
}

fn entry_key(ws: &str, id: OpId) -> String {
    format!("oplog/{}/op/{id:020}", escape(ws))
}

impl<K: Kv> KvOpLog<K> {
    async fn head(&self, ws: &str) -> Result<(OpId, Option<u64>)> {
        match self.kv.get(&head_key(ws)).await? {
            None => Ok((0, None)),
            Some((bytes, rev)) => {
                let s = std::str::from_utf8(&bytes).map_err(VcsError::storage)?;
                let n = s
                    .parse::<OpId>()
                    .map_err(|_| VcsError::Storage(format!("oplog head of {ws} is {s:?}")))?;
                Ok((n, Some(rev)))
            }
        }
    }

    /// The last id that exists, starting the walk at the head.
    async fn tail(&self, ws: &str) -> Result<OpId> {
        let (mut n, _) = self.head(ws).await?;
        while self.kv.get(&entry_key(ws, n + 1)).await?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    /// Move the head forwards to at least `target`; never backwards.
    async fn advance_head(&self, ws: &str, target: OpId) -> Result<()> {
        for _ in 0..APPEND_TRIES {
            let (h, rev) = self.head(ws).await?;
            if h >= target {
                return Ok(());
            }
            let bytes = target.to_string().into_bytes();
            if self.kv.cas(&head_key(ws), rev, bytes).await?.is_ok() {
                return Ok(());
            }
        }
        // The entry is written and readers walk past a lagging head, so this is
        // not a lost op — but it should not happen, so say so.
        Err(VcsError::Storage(format!("oplog head of {ws} would not advance to {target}")))
    }
}

impl<K: Kv> OpLog for KvOpLog<K> {
    async fn append(&self, ws: &str, op: NewOp) -> Result<OpEntry> {
        for _ in 0..APPEND_TRIES {
            let id = self.tail(ws).await? + 1;
            let entry = OpEntry {
                id,
                workspace: ws.to_string(),
                at: op.at,
                agent: op.agent.clone(),
                kind: op.kind.clone(),
                moves: op.moves.clone(),
            };
            let bytes = serde_json::to_vec(&entry).map_err(VcsError::storage)?;
            match self.kv.cas(&entry_key(ws, id), None, bytes).await? {
                Ok(_) => {
                    self.advance_head(ws, id).await?;
                    return Ok(entry);
                }
                Err(_) => continue, // somebody else took `id`; go again
            }
        }
        Err(VcsError::Storage(format!("oplog append to {ws} lost {APPEND_TRIES} races")))
    }

    async fn get(&self, ws: &str, id: OpId) -> Result<Option<OpEntry>> {
        match self.kv.get(&entry_key(ws, id)).await? {
            None => Ok(None),
            Some((bytes, _)) => serde_json::from_slice(&bytes).map(Some).map_err(VcsError::storage),
        }
    }

    async fn list(&self, ws: &str, after: Option<OpId>, limit: u32) -> Result<Vec<OpEntry>> {
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
    fn append(&self, ws: &str, op: NewOp) -> impl Future<Output = Result<OpEntry>> + Send {
        (**self).append(ws, op)
    }
    fn get(&self, ws: &str, id: OpId) -> impl Future<Output = Result<Option<OpEntry>>> + Send {
        (**self).get(ws, id)
    }
    fn list(
        &self,
        ws: &str,
        after: Option<OpId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<OpEntry>>> + Send {
        (**self).list(ws, after, limit)
    }
    fn latest(&self, ws: &str) -> impl Future<Output = Result<Option<OpId>>> + Send {
        (**self).latest(ws)
    }
}
