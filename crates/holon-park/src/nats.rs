//! NATS JetStream adapter (`native` feature): ticket records and their two
//! indexes in one KV bucket, and `PARK_WAKE` — a work-queue stream a resumer
//! pulls from to learn a ticket is `ready`, the same shape as `comp-media`'s
//! `MEDIA_JOBS` (`RetentionPolicy::WorkQueue`, a durable pull consumer;
//! ADR-0100).
//!
//! # Keys
//!
//! One bucket, three key shapes. Unlike `holon_vcs::nats`, a caller-supplied
//! string is always hashed rather than escaped — none of these three is ever
//! read back out of the key itself, so there is nothing to preserve:
//!
//! * `ticket.<ticket>` — the [`Record`], JSON. `ticket` is already a
//!   lower-case hex SHA-256 ([`crate::ticket::ticket_id`]).
//! * `correlation.<sha256 of the correlation>` — the ticket it was claimed
//!   for, as UTF-8 bytes.
//! * `session.<sha256 of the session>` — the session's ticket list, JSON,
//!   grown by compare-and-set. There is no bucket-wide `keys()` scan here the
//!   way `holon-vcs` needs one for startup repair: a session's own list IS
//!   the index, so [`NatsParkStore::list_session`] costs one read, never a
//!   scan of every ticket ever parked.
//!
//! # The notification is not the durability boundary
//!
//! `PARK_WAKE` tells a resumer pool "look at this ticket, don't poll" — it is
//! an optimization. The ticket record in the KV bucket is the actual answer;
//! `pending`/`oplog` still find a `ready` ticket even if a publish to
//! `PARK_WAKE` fails or nobody is pulling from it yet. `comp-park` (the
//! daemon, not this module) is what decides to publish, and only once per
//! real transition (`Engine::wake`'s `WakeOutcome::freshly_woken`) — never on
//! a redelivered webhook's idempotent no-op.

use async_nats::jetstream::{self, kv, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ParkError, Result};
use crate::model::{SessionId, TicketId};
use crate::store::{ParkStore, Record, Revision};

/// How many times a session's ticket-list compare-and-set is retried before
/// giving up. Contention here is one session's own concurrent `park` calls —
/// the same small, bounded contention `crate::engine`'s own retry loop
/// assumes, never a hot loop.
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
/// A caller of this crate over its `native` feature (`comp-park`) is meant to
/// go through THIS function and never import `async-nats` itself — the same
/// shape `holon_vcs::nats::connect` gives `comp-vcs`. Two crates each pinning
/// their own `async-nats` version (this workspace's `reconciler` binary crate
/// pins 0.49 for its own NATS use; this crate's workspace deps pin 0.50) makes
/// `async_nats::jetstream::Context` two DIFFERENT nominal types even though
/// they are structurally identical — a value of one cannot be handed to a
/// function expecting the other. Keeping every `async_nats` type inside this
/// module (and `holon_vcs::nats`'s, independently) is what lets both engines
/// live in one binary without their two `async-nats`es ever having to agree.
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
    /// Open `bucket`, creating it (history 1 — a ticket's past is the oplog
    /// read, not this bucket's) if it does not exist yet.
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self> {
        let kv = match js.get_key_value(bucket).await {
            Ok(store) => store,
            Err(_) => {
                let cfg =
                    kv::Config { bucket: bucket.to_string(), history: 1, ..Default::default() };
                match js.create_key_value(cfg).await {
                    Ok(store) => store,
                    // Somebody else created it in between.
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

    /// A read-modify-CAS-write loop over the session's own ticket list — the
    /// same shape as `crate::engine`'s retries, just one layer lower, since
    /// there is no `Engine` above this call to retry it.
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

/// What lands on `PARK_WAKE`: enough for a resumer to act without another
/// round trip for the session id, and nothing else — the ticket record
/// itself, not this message, is the answer's source of truth.
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
    /// Open `name` as a `WorkQueue` stream, creating it if missing — the same
    /// `get_or_create_stream` call `comp-media` opens `MEDIA_JOBS` with. Its
    /// one subject is `<name>.ticket`; a resumer's durable pull consumer names
    /// that same subject as its `filter_subject`.
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

    /// Tell a resumer a ticket is `ready`. Waits for the JetStream ack, so a
    /// caller that gets `Ok` knows the message is durably queued — not just
    /// sent.
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
