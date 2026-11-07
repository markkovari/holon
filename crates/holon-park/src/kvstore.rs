//! [`ParkStore`] over any compare-and-set keyvalue: a record per ticket and batch, an index list
//! per session and per group, with `comp-park`'s key layout (ADR-0132). `components/park-store`
//! gives it the app's keyvalue (`comp:store/cas`); [`MemKv`] is the same contract in memory, so
//! the crash matrix runs over exactly what the component runs.

use std::collections::BTreeMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::error::{ParkError, Result};
use crate::model::TicketId;
use crate::store::{Batch, IndexChild, ParkStore, Record, Revision};

/// A group's children may race a thousand at once for one list; there is no sleep in a guest,
/// so a lost race is simply read and tried again.
const MAX_INDEX_RETRIES: u32 = 64;

/// What a keyvalue with compare-and-set answers. Every call has finished when it returns.
pub trait Kv: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>>;
    /// `expected` of 0 is "must not exist yet". `Ok(false)`: the key has moved, the write lost.
    fn set(&self, key: &str, value: &[u8], expected: Revision) -> Result<bool>;
    fn keys(&self) -> Result<Vec<String>>;
}

/// One revision counter across all keys, so a revision is never reused.
type Entries = (Revision, BTreeMap<String, (Vec<u8>, Revision)>);

#[derive(Default)]
pub struct MemKv(Mutex<Entries>);

impl MemKv {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Kv for MemKv {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        Ok(self.0.lock().unwrap().1.get(key).cloned())
    }
    fn set(&self, key: &str, value: &[u8], expected: Revision) -> Result<bool> {
        let mut g = self.0.lock().unwrap();
        if g.1.get(key).map_or(0, |(_, r)| *r) != expected {
            return Ok(false);
        }
        g.0 += 1;
        let rev = g.0;
        g.1.insert(key.to_string(), (value.to_vec(), rev));
        Ok(true)
    }
    fn keys(&self) -> Result<Vec<String>> {
        Ok(self.0.lock().unwrap().1.keys().cloned().collect())
    }
}

impl<K: Kv> Kv for std::sync::Arc<K> {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        (**self).get(key)
    }
    fn set(&self, key: &str, value: &[u8], expected: Revision) -> Result<bool> {
        (**self).set(key, value, expected)
    }
    fn keys(&self) -> Result<Vec<String>> {
        (**self).keys()
    }
}

/// Caller-supplied strings are hashed, never escaped: none is read back out of a key.
fn hashed(prefix: &str, s: &str) -> String {
    format!("{prefix}.{}", hex::encode(Sha256::digest(s.as_bytes())))
}

fn ticket_key(ticket: &str) -> String {
    format!("ticket.{ticket}")
}

/// `id` is already a hex SHA-256 (`ticket::batch_id`), so it is a safe key as it is.
fn batch_key(id: &str) -> String {
    format!("batch.{id}")
}

fn storage(e: impl std::fmt::Display) -> ParkError {
    ParkError::storage(e.to_string())
}

pub struct KvParkStore<K>(pub K);

impl<K: Kv> KvParkStore<K> {
    fn read<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<(T, Revision)>> {
        match self.0.get(key)? {
            Some((v, rev)) => Ok(Some((serde_json::from_slice(&v).map_err(storage)?, rev))),
            None => Ok(None),
        }
    }

    fn write<T: serde::Serialize>(&self, key: &str, value: &T, expected: Revision) -> Result<bool> {
        self.0.set(key, &serde_json::to_vec(value).map_err(storage)?, expected)
    }

    /// The bound and the write are one CAS, so racers for the last place cannot both have it.
    fn grow(&self, key: &str, item: &str, max: Option<usize>, what: &str) -> Result<IndexChild> {
        for _ in 0..MAX_INDEX_RETRIES {
            let (mut list, expected): (Vec<TicketId>, Revision) =
                self.read(key)?.unwrap_or((Vec::new(), 0));
            if list.iter().any(|t| t == item) {
                return Ok(IndexChild::Present);
            }
            if max.is_some_and(|m| list.len() >= m) {
                return Ok(IndexChild::Full);
            }
            list.push(item.to_string());
            if self.write(key, &list, expected)? {
                return Ok(IndexChild::Added);
            }
        }
        Err(ParkError::storage(format!("{what}: repeated compare-and-set contention")))
    }

    fn keys_with(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .0
            .keys()?
            .into_iter()
            .filter_map(|k| k.strip_prefix(prefix).map(String::from))
            .collect())
    }
}

impl<K: Kv> ParkStore for KvParkStore<K> {
    async fn get(&self, ticket: &str) -> Result<Option<(Record, Revision)>> {
        self.read(&ticket_key(ticket))
    }

    async fn create(&self, ticket: &str, record: &Record) -> Result<bool> {
        self.write(&ticket_key(ticket), record, 0)
    }

    async fn cas(&self, ticket: &str, expected: Revision, record: &Record) -> Result<bool> {
        self.write(&ticket_key(ticket), record, expected)
    }

    async fn claim_correlation(&self, correlation: &str, ticket: &str) -> Result<Option<TicketId>> {
        if self.write(&hashed("correlation", correlation), &ticket, 0)? {
            return Ok(None);
        }
        self.find_correlation(correlation).await
    }

    async fn find_correlation(&self, correlation: &str) -> Result<Option<TicketId>> {
        Ok(self.read::<TicketId>(&hashed("correlation", correlation))?.map(|(t, _)| t))
    }

    async fn index_session(&self, session: &str, ticket: &str) -> Result<()> {
        let what = format!("session {session:?}'s ticket index");
        self.grow(&hashed("session", session), ticket, None, &what).map(|_| ())
    }

    async fn index_child(&self, parent: &str, child: &str, max: u32) -> Result<IndexChild> {
        let what = format!("group {parent}'s child index");
        self.grow(&hashed("children", parent), child, Some(max as usize), &what)
    }

    async fn list_children(&self, parent: &str) -> Result<Vec<TicketId>> {
        Ok(self.read(&hashed("children", parent))?.map(|(l, _)| l).unwrap_or_default())
    }

    async fn list_session(&self, session: &str) -> Result<Vec<TicketId>> {
        Ok(self.read(&hashed("session", session))?.map(|(l, _)| l).unwrap_or_default())
    }

    async fn create_batch(&self, id: &str, batch: &Batch) -> Result<bool> {
        self.write(&batch_key(id), batch, 0)
    }

    async fn get_batch(&self, id: &str) -> Result<Option<(Batch, Revision)>> {
        self.read(&batch_key(id))
    }

    async fn cas_batch(&self, id: &str, expected: Revision, batch: &Batch) -> Result<bool> {
        self.write(&batch_key(id), batch, expected)
    }

    async fn list_batches(&self) -> Result<Vec<String>> {
        self.keys_with("batch.")
    }

    async fn list_tickets(&self) -> Result<Vec<TicketId>> {
        self.keys_with("ticket.")
    }
}
