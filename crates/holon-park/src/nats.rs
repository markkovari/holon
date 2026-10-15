//! NATS JetStream adapter (`native` feature): ticket records and two indexes in
//! one KV bucket, plus `PARK_WAKE` — a work-queue stream a resumer pulls from
//! (same shape as `comp-media`'s `MEDIA_JOBS`; ADR-0100).
//!
//! # Keys
//!
//! Caller-supplied strings are always hashed, never escaped: none is read back
//! out of the key.
//!
//! * `ticket.<ticket>` — the [`Record`], JSON (`ticket` is already hex SHA-256).
//! * `correlation.<sha256>` — the ticket it was claimed for, UTF-8 bytes.
//! * `session.<sha256>` — the session's ticket list, JSON, grown by CAS. It is
//!   the index itself, so [`NatsParkStore::list_session`] is one read, no scan.
//!
//! # The notification is not the durability boundary
//!
//! `PARK_WAKE` is an optimization ("look at this ticket, don't poll"); the KV
//! record is the answer. `pending`/`oplog` still find a `ready` ticket if a
//! publish fails. `comp-park` publishes once per real transition
//! (`WakeOutcome::freshly_woken`), never on a redelivered webhook.

use async_nats::jetstream::{self, kv, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ParkError, Result};
use crate::model::{SessionId, TicketId};
use crate::store::{ParkStore, Record, Revision};

/// Retry bound for a session's ticket-list CAS; contention is one session's own
/// concurrent `park` calls.
const MAX_INDEX_RETRIES: u32 = 8;

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn ticket_key(ticket: &str) -> String {
    format!("ticket.{ticket}")
}

fn correlation_key(correlation: &str) -> String {
    format!("correlation.{}", sha256_hex(correlation))
}

fn session_key(session: &str) -> String {
    format!("session.{}", sha256_hex(session))
}

/// Connect and open (creating if needed) the KV bucket and the `PARK_WAKE`
/// stream.
///
/// Callers go through this and never import `async-nats` themselves: two crates
/// pinning different `async-nats` versions make `jetstream::Context` two
/// different nominal types, so every `async_nats` type stays inside this module
/// (and `holon_vcs::nats`) so both engines can live in one binary.
pub async fn connect(
    url: &str,
    bucket: &str,
    wake_stream_name: &str,
) -> Result<(NatsParkStore, WakeStream)> {
    let client = async_nats::connect(url).await.map_err(ParkError::storage)?;
    let js = jetstream::new(client);
    let store = NatsParkStore::open(&js, bucket).await?;
    let wake = WakeStream::open(&js, wake_stream_name).await?;
    Ok((store, wake))
}

pub struct NatsParkStore {
    kv: kv::Store,
}

impl NatsParkStore {
    /// Open `bucket`, creating it (history 1: a ticket's past is the oplog) if
    /// missing.
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self> {
        let kv = match js.get_key_value(bucket).await {
            Ok(store) => store,
            Err(_) => {
                let cfg =
                    kv::Config { bucket: bucket.to_string(), history: 1, ..Default::default() };
                match js.create_key_value(cfg).await {
                    Ok(store) => store,
                    Err(e) => js.get_key_value(bucket).await.map_err(|_| ParkError::storage(e))?,
                }
            }
        };
        Ok(NatsParkStore { kv })
    }
}

impl ParkStore for NatsParkStore {
    async fn get(&self, ticket: &str) -> Result<Option<(Record, Revision)>> {
        match self.kv.entry(ticket_key(ticket)).await.map_err(ParkError::storage)? {
            Some(e) if e.operation == kv::Operation::Put => {
                let record: Record =
                    serde_json::from_slice(&e.value).map_err(ParkError::storage)?;
                Ok(Some((record, e.revision)))
            }
            _ => Ok(None),
        }
    }

    async fn create(&self, ticket: &str, record: &Record) -> Result<bool> {
        let value = serde_json::to_vec(record).map_err(ParkError::storage)?;
        match self.kv.create(ticket_key(ticket), value.into()).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(ParkError::storage(e)),
        }
    }

    async fn cas(&self, ticket: &str, expected: Revision, record: &Record) -> Result<bool> {
        let value = serde_json::to_vec(record).map_err(ParkError::storage)?;
        match self.kv.update(ticket_key(ticket), value.into(), expected).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == kv::UpdateErrorKind::WrongLastRevision => Ok(false),
            Err(e) => Err(ParkError::storage(e)),
        }
    }

    async fn claim_correlation(&self, correlation: &str, ticket: &str) -> Result<Option<TicketId>> {
        match self.kv.create(correlation_key(correlation), ticket.as_bytes().to_vec().into()).await
        {
            Ok(_) => Ok(None),
            Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => {
                self.find_correlation(correlation).await
            }
            Err(e) => Err(ParkError::storage(e)),
        }
    }

    async fn find_correlation(&self, correlation: &str) -> Result<Option<TicketId>> {
        match self.kv.get(correlation_key(correlation)).await.map_err(ParkError::storage)? {
            Some(bytes) => Ok(Some(String::from_utf8(bytes.to_vec()).map_err(ParkError::storage)?)),
            None => Ok(None),
        }
    }

    /// Read-modify-CAS-write loop over the session's ticket list; no `Engine`
    /// sits above to retry it.
    async fn index_session(&self, session: &str, ticket: &str) -> Result<()> {
        let key = session_key(session);
        for _ in 0..MAX_INDEX_RETRIES {
            let entry = self.kv.entry(&key).await.map_err(ParkError::storage)?;
            let (mut list, expected): (Vec<TicketId>, Option<Revision>) = match &entry {
                Some(e) if e.operation == kv::Operation::Put => (
                    serde_json::from_slice(&e.value).map_err(ParkError::storage)?,
                    Some(e.revision),
                ),
                _ => (Vec::new(), None),
            };
            if list.iter().any(|t| t == ticket) {
                return Ok(()); // already indexed
            }
            list.push(ticket.to_string());
            let value = serde_json::to_vec(&list).map_err(ParkError::storage)?;
            let written = match expected {
                None => match self.kv.create(&key, value.into()).await {
                    Ok(_) => true,
                    Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => false,
                    Err(e) => return Err(ParkError::storage(e)),
                },
                Some(rev) => match self.kv.update(&key, value.into(), rev).await {
                    Ok(_) => true,
                    Err(e) if e.kind() == kv::UpdateErrorKind::WrongLastRevision => false,
                    Err(e) => return Err(ParkError::storage(e)),
                },
            };
            if written {
                return Ok(());
            }
        }
        Err(ParkError::storage(format!(
            "session {session:?}'s ticket index: repeated compare-and-set contention"
        )))
    }

    async fn list_session(&self, session: &str) -> Result<Vec<TicketId>> {
        match self.kv.get(session_key(session)).await.map_err(ParkError::storage)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(ParkError::storage),
            None => Ok(Vec::new()),
        }
    }
}

/// What lands on `PARK_WAKE`: enough for a resumer to act; the ticket record,
/// not this message, is the source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeMessage {
    pub session: SessionId,
    pub ticket: TicketId,
}

pub struct WakeStream {
    js: jetstream::Context,
    subject: String,
}

impl WakeStream {
    /// Open `name` as a `WorkQueue` stream, creating it if missing. Its one
    /// subject is `<name>.ticket`, the `filter_subject` of a resumer's durable
    /// pull consumer.
    pub async fn open(js: &jetstream::Context, name: &str) -> Result<Self> {
        let subject = format!("{name}.ticket");
        js.get_or_create_stream(stream::Config {
            name: name.to_string(),
            subjects: vec![subject.clone()],
            retention: stream::RetentionPolicy::WorkQueue,
            ..Default::default()
        })
        .await
        .map_err(ParkError::storage)?;
        Ok(WakeStream { js: js.clone(), subject })
    }

    /// Tell a resumer a ticket is `ready`. Waits for the JetStream ack, so `Ok`
    /// means durably queued.
    pub async fn publish(&self, msg: &WakeMessage) -> Result<()> {
        let payload = serde_json::to_vec(msg).map_err(ParkError::storage)?;
        let ack = self
            .js
            .publish(self.subject.clone(), payload.into())
            .await
            .map_err(ParkError::storage)?;
        ack.await.map_err(ParkError::storage)?;
        Ok(())
    }
}
