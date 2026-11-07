//! `holon_park::store::ParkStore` over this app's keyvalue: a record per ticket and batch, moved
//! by `comp:store/cas`, with `comp-park`'s NATS key layout (ADR-0132). Every call is one host call
//! and finishes at once, so the engine's futures are driven by [`block_on`].

use std::future::Future;

use holon_park::error::{ParkError, Result};
use holon_park::model::TicketId;
use holon_park::store::{Batch, IndexChild, ParkStore, Record, Revision};
use sha2::{Digest, Sha256};

use crate::bindings::comp::store::cas;
use crate::bindings::wasi::keyvalue::store;

/// A group's children may race a thousand at once for one list; there is no sleep in a guest,
/// so a lost race is simply read and tried again.
const MAX_INDEX_RETRIES: u32 = 64;

/// The engine's futures never wait on anything: each step is a host call that has returned.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// Caller-supplied strings are hashed, never escaped: none is read back out of a key.
fn hashed(prefix: &str, s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}.{hex}")
}

fn ticket_key(ticket: &str) -> String {
    format!("ticket.{ticket}")
}

/// `id` is already a hex SHA-256 (`ticket::batch_id`), so it is a safe key as it is.
fn batch_key(id: &str) -> String {
    format!("batch.{id}")
}

fn bucket() -> Result<store::Bucket> {
    store::open("default").map_err(|e| ParkError::storage(format!("{e:?}")))
}

fn storage(e: impl std::fmt::Debug) -> ParkError {
    ParkError::storage(format!("{e:?}"))
}

#[derive(Clone, Copy)]
pub struct KvStore;

impl KvStore {
    fn read<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<(T, Revision)>> {
        match cas::get(&bucket()?, key).map_err(storage)? {
            Some(v) => Ok(Some((serde_json::from_slice(&v.value).map_err(storage)?, v.revision))),
            None => Ok(None),
        }
    }

    /// `expected` of 0 is "must not exist yet". `Ok(false)`: lost the race.
    fn write<T: serde::Serialize>(&self, key: &str, value: &T, expected: Revision) -> Result<bool> {
        let bytes = serde_json::to_vec(value).map_err(storage)?;
        match cas::set(&bucket()?, key, &bytes, expected).map_err(storage)? {
            cas::Outcome::Committed(_) => Ok(true),
            cas::Outcome::Conflict(_) => Ok(false),
        }
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
        let b = bucket()?;
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = b.list_keys(cursor).map_err(storage)?;
            out.extend(
                page.keys.into_iter().filter_map(|k| k.strip_prefix(prefix).map(String::from)),
            );
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }
}

impl ParkStore for KvStore {
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
