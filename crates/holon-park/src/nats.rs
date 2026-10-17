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
//! * `children.<sha256>` — a group's child list (ADR-0109), the same shape, grown
//!   by the same loop with a bound on how long it may get.
//! * `batch.<id>` — a commit's intent (ADR-0110), JSON, moved by CAS like a ticket.
//!
//! # The notification is not the durability boundary
//!
//! `PARK_WAKE` is an optimization ("look at this ticket, don't poll"); the KV
//! record is the answer. `pending`/`oplog` still find a `ready` ticket if a
//! publish fails. `comp-park` publishes once per real transition
//! (`WakeOutcome::freshly_woken`), never on a redelivered webhook.

use std::time::Duration;

use async_nats::jetstream::consumer::{pull, AckPolicy};
use async_nats::jetstream::message::AckKind;
use async_nats::jetstream::{self, kv, stream};
use futures::{StreamExt, TryStreamExt};
use sha2::{Digest, Sha256};

use crate::error::{ParkError, Result};
use crate::model::TicketId;
pub use crate::model::{WakeKind, WakeMessage};
use crate::store::{Batch, IndexChild, ParkStore, Record, Revision};

/// Retry bound for a list's CAS. A session's list is contended by that session's own concurrent
/// `park` calls, and a group's (ADR-0109) by a fan-out registering its children at once, which
/// is the point of one: a thousand children may race for one list. Each lost race backs off a
/// few milliseconds so the racers spread out instead of colliding again together.
const MAX_INDEX_RETRIES: u32 = 64;

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

fn children_key(parent: &str) -> String {
    format!("children.{}", sha256_hex(parent))
}

/// `id` is already a hex SHA-256 (`ticket::batch_id`), so it is a safe key as it is.
fn batch_key(id: &str) -> String {
    format!("batch.{id}")
}

/// The most copies JetStream keeps of a stream.
pub const MAX_REPLICAS: usize = 5;

/// What was asked for has to be something a stream can be.
fn check_asked(replicas: usize) -> Result<()> {
    if (1..=MAX_REPLICAS).contains(&replicas) {
        return Ok(());
    }
    Err(ParkError::Invalid(format!("replicas is 1 to {MAX_REPLICAS}, not {replicas}")))
}

/// `get_key_value` and `get_or_create_stream` hand back what is already there, whatever it
/// was made with, so asking for three copies of a stream that has one would be a flag that
/// changes nothing while saying it did. It is refused instead, with the command that raises
/// it. More copies than were asked for is fine: the number is a floor, so a process left on
/// the default never lowers what another asked for.
///
/// This does not raise it for the caller. Going from one copy to three moves the stream's whole
/// contents across the cluster, which is the operator's to schedule and not a side effect of
/// whichever process happened to start first.
fn check_kept(what: &str, have: usize, want: usize, remedy: &str) -> Result<()> {
    if have >= want {
        return Ok(());
    }
    let copies = if have == 1 { "copy" } else { "copies" };
    Err(ParkError::Storage(format!(
        "{what} is kept in {have} {copies} and {want} were asked for; raise it first, with \
         `{remedy}`"
    )))
}

/// One server or several, comma-separated. A stray comma is refused: `async-nats` accepts an
/// empty address (an empty host, port 4222) as a server like any other, so a typo in a list
/// would become a server nobody named and not an error.
fn server_list(url: &str) -> Result<Vec<async_nats::ServerAddr>> {
    url.split(',')
        .map(|one| match one.trim() {
            "" => Err(ParkError::Invalid(format!("NATS url `{url}` has an empty address in it"))),
            one => one.parse().map_err(|e| ParkError::Invalid(format!("NATS url `{url}`: {e}"))),
        })
        .collect()
}

/// Connect and open (creating if needed) the KV bucket and the `PARK_WAKE`
/// stream, each kept in one copy.
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
    connect_with_replicas(url, bucket, wake_stream_name, 1).await
}

/// [`connect`], with the bucket and the stream kept in `replicas` copies (ADR-0111).
///
/// `url` is one server or several, comma-separated: a client that was only told about one
/// cannot start while that one is down, however many copies there are of what it would read.
/// A client given all of them reconnects to whichever is up when the one it was talking to dies.
pub async fn connect_with_replicas(
    url: &str,
    bucket: &str,
    wake_stream_name: &str,
    replicas: usize,
) -> Result<(NatsParkStore, WakeStream)> {
    check_asked(replicas)?;
    let servers = server_list(url)?;
    let client = async_nats::connect(servers).await.map_err(ParkError::storage)?;
    let js = jetstream::new(client);
    let store = NatsParkStore::open_with_replicas(&js, bucket, replicas).await?;
    let wake = WakeStream::open_with_replicas(&js, wake_stream_name, replicas).await?;
    Ok((store, wake))
}

pub struct NatsParkStore {
    kv: kv::Store,
}

impl NatsParkStore {
    /// Open `bucket`, creating it (history 1: a ticket's past is the oplog) if
    /// missing, in one copy.
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self> {
        Self::open_with_replicas(js, bucket, 1).await
    }

    /// [`open`](Self::open), creating the bucket in `replicas` copies. A bucket that already
    /// exists is used as it is, and is refused if it keeps fewer copies than that (see
    /// `check_kept`).
    pub async fn open_with_replicas(
        js: &jetstream::Context,
        bucket: &str,
        replicas: usize,
    ) -> Result<Self> {
        check_asked(replicas)?;
        let kv = match js.get_key_value(bucket).await {
            Ok(store) => store,
            Err(_) => {
                let cfg = kv::Config {
                    bucket: bucket.to_string(),
                    history: 1,
                    num_replicas: replicas,
                    ..Default::default()
                };
                match js.create_key_value(cfg).await {
                    Ok(store) => store,
                    Err(e) => js.get_key_value(bucket).await.map_err(|_| {
                        ParkError::storage(format!(
                            "creating bucket `{bucket}` in {replicas} {}: {e}",
                            if replicas == 1 { "copy" } else { "copies" }
                        ))
                    })?,
                }
            }
        };
        // A bucket's own stream is edited through `kv edit`: `stream edit` asks for a
        // confirmation that `--force` does not skip, and so fails where there is no terminal.
        check_kept(
            &format!("bucket `{bucket}`"),
            kv.stream.cached_info().config.num_replicas,
            replicas,
            &format!("nats kv edit {bucket} --replicas {replicas}"),
        )?;
        Ok(NatsParkStore { kv })
    }

    /// Add `item` to the JSON list at `key`, if it is not there already and, when
    /// `max` is set, only while the list has room: one read, then a create or a
    /// compare-and-set, retried when it loses the race. The bound and the write are
    /// one step, so racers for the last place cannot both have it.
    async fn grow(
        &self,
        key: &str,
        item: &str,
        max: Option<usize>,
        what: impl Fn() -> String,
    ) -> Result<IndexChild> {
        for attempt in 0..MAX_INDEX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(u64::from(attempt).min(8))).await;
            }
            let entry = self.kv.entry(key).await.map_err(ParkError::storage)?;
            let (mut list, expected): (Vec<TicketId>, Option<Revision>) = match &entry {
                Some(e) if e.operation == kv::Operation::Put => (
                    serde_json::from_slice(&e.value).map_err(ParkError::storage)?,
                    Some(e.revision),
                ),
                _ => (Vec::new(), None),
            };
            if list.iter().any(|t| t == item) {
                return Ok(IndexChild::Present);
            }
            if max.is_some_and(|m| list.len() >= m) {
                return Ok(IndexChild::Full);
            }
            list.push(item.to_string());
            let value = serde_json::to_vec(&list).map_err(ParkError::storage)?;
            let written = match expected {
                None => match self.kv.create(key, value.into()).await {
                    Ok(_) => true,
                    Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => false,
                    Err(e) => return Err(ParkError::storage(e)),
                },
                Some(rev) => match self.kv.update(key, value.into(), rev).await {
                    Ok(_) => true,
                    Err(e) if e.kind() == kv::UpdateErrorKind::WrongLastRevision => false,
                    Err(e) => return Err(ParkError::storage(e)),
                },
            };
            if written {
                return Ok(IndexChild::Added);
            }
        }
        Err(ParkError::storage(format!("{}: repeated compare-and-set contention", what())))
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
        self.grow(&session_key(session), ticket, None, || {
            format!("session {session:?}'s ticket index")
        })
        .await
        .map(|_| ())
    }

    async fn index_child(&self, parent: &str, child: &str, max: u32) -> Result<IndexChild> {
        self.grow(&children_key(parent), child, Some(max as usize), || {
            format!("group {parent}'s child index")
        })
        .await
    }

    async fn list_children(&self, parent: &str) -> Result<Vec<TicketId>> {
        match self.kv.get(children_key(parent)).await.map_err(ParkError::storage)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(ParkError::storage),
            None => Ok(Vec::new()),
        }
    }

    async fn list_session(&self, session: &str) -> Result<Vec<TicketId>> {
        match self.kv.get(session_key(session)).await.map_err(ParkError::storage)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(ParkError::storage),
            None => Ok(Vec::new()),
        }
    }

    async fn create_batch(&self, id: &str, batch: &Batch) -> Result<bool> {
        let value = serde_json::to_vec(batch).map_err(ParkError::storage)?;
        match self.kv.create(batch_key(id), value.into()).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(ParkError::storage(e)),
        }
    }

    async fn get_batch(&self, id: &str) -> Result<Option<(Batch, Revision)>> {
        match self.kv.entry(batch_key(id)).await.map_err(ParkError::storage)? {
            Some(e) if e.operation == kv::Operation::Put => {
                let batch: Batch = serde_json::from_slice(&e.value).map_err(ParkError::storage)?;
                Ok(Some((batch, e.revision)))
            }
            _ => Ok(None),
        }
    }

    async fn cas_batch(&self, id: &str, expected: Revision, batch: &Batch) -> Result<bool> {
        let value = serde_json::to_vec(batch).map_err(ParkError::storage)?;
        match self.kv.update(batch_key(id), value.into(), expected).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == kv::UpdateErrorKind::WrongLastRevision => Ok(false),
            Err(e) => Err(ParkError::storage(e)),
        }
    }

    async fn list_batches(&self) -> Result<Vec<String>> {
        let keys = self.kv.keys().await.map_err(ParkError::storage)?;
        let keys: Vec<String> = keys.try_collect().await.map_err(ParkError::storage)?;
        Ok(keys.into_iter().filter_map(|k| k.strip_prefix("batch.").map(str::to_string)).collect())
    }

    /// One pass over the bucket's keys, keeping the `ticket.` ones. Reads keys
    /// only, never a value; the engine fetches each record it wants.
    async fn list_tickets(&self) -> Result<Vec<TicketId>> {
        let keys = self.kv.keys().await.map_err(ParkError::storage)?;
        let keys: Vec<String> = keys.try_collect().await.map_err(ParkError::storage)?;
        Ok(keys.into_iter().filter_map(|k| k.strip_prefix("ticket.").map(str::to_string)).collect())
    }
}

pub struct WakeStream {
    js: jetstream::Context,
    name: String,
    subject: String,
}

impl WakeStream {
    /// Open `name` as a `WorkQueue` stream, creating it if missing, in one copy. Its one
    /// subject is `<name>.ticket`, the `filter_subject` of a resumer's durable
    /// pull consumer.
    pub async fn open(js: &jetstream::Context, name: &str) -> Result<Self> {
        Self::open_with_replicas(js, name, 1).await
    }

    /// [`open`](Self::open), creating the stream in `replicas` copies. A stream that already
    /// exists is used as it is, and is refused if it keeps fewer copies than that.
    ///
    /// The stream is only a nudge — the record is the answer, and a recovery pass finds a ticket
    /// whose announcement was lost — so losing it costs latency, not tickets. It is kept in the
    /// same number of copies anyway: with one, the node holding it going down stops every
    /// announcement and delivery until it is back, and the copies of the bucket would protect
    /// the tickets and not the service.
    pub async fn open_with_replicas(
        js: &jetstream::Context,
        name: &str,
        replicas: usize,
    ) -> Result<Self> {
        check_asked(replicas)?;
        let subject = format!("{name}.ticket");
        let stream = js
            .get_or_create_stream(stream::Config {
                name: name.to_string(),
                subjects: vec![subject.clone()],
                retention: stream::RetentionPolicy::WorkQueue,
                num_replicas: replicas,
                ..Default::default()
            })
            .await
            .map_err(|e| {
                ParkError::storage(format!(
                    "opening stream `{name}` in {replicas} {}: {e}",
                    if replicas == 1 { "copy" } else { "copies" }
                ))
            })?;
        check_kept(
            &format!("stream `{name}`"),
            stream.cached_info().config.num_replicas,
            replicas,
            &format!("nats stream edit {name} --replicas {replicas} --force"),
        )?;
        Ok(WakeStream { js: js.clone(), name: name.to_string(), subject })
    }

    /// Tell a resumer something about a ticket. Waits for the JetStream ack, so
    /// `Ok` means durably queued.
    ///
    /// The message id (`Nats-Msg-Id`) names the ticket and what is being said,
    /// so announcing the same thing twice inside the stream's duplicate window
    /// queues it once: a recovery pass and a `wake` that both announce one
    /// answer do not hand a resumer two copies.
    pub async fn publish(&self, msg: &WakeMessage) -> Result<()> {
        let payload = serde_json::to_vec(msg).map_err(ParkError::storage)?;
        let id = match msg.kind {
            WakeKind::Resolved => format!("{}.resolved", msg.ticket),
            WakeKind::Redispatch { attempt } => format!("{}.redispatch.{attempt}", msg.ticket),
        };
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
        let ack = self
            .js
            .publish_with_headers(self.subject.clone(), headers, payload.into())
            .await
            .map_err(ParkError::storage)?;
        ack.await.map_err(ParkError::storage)?;
        Ok(())
    }
}

impl WakeStream {
    /// A durable pull consumer on this stream, created if missing. Every
    /// resumer that opens the same `name` shares one position in the stream, so
    /// a message goes to one of them, not all.
    ///
    /// `ack_wait` is how long a delivered message may go unanswered before the
    /// server hands it to someone else: it must outlast one sink call.
    pub async fn consumer(&self, name: &str, ack_wait: Duration) -> Result<WakeConsumer> {
        let stream = self.js.get_stream(&self.name).await.map_err(ParkError::storage)?;
        let consumer = stream
            .get_or_create_consumer(
                name,
                pull::Config {
                    durable_name: Some(name.to_string()),
                    ack_policy: AckPolicy::Explicit,
                    ack_wait,
                    filter_subject: self.subject.clone(),
                    ..Default::default()
                },
            )
            .await
            .map_err(ParkError::storage)?;
        let messages = consumer.messages().await.map_err(ParkError::storage)?;
        Ok(WakeConsumer { messages })
    }
}

impl crate::resume::Announcer for WakeStream {
    async fn announce(&self, msg: &WakeMessage) -> Result<()> {
        self.publish(msg).await
    }
}

/// A resumer's view of `PARK_WAKE`: messages, one at a time, each to be
/// acknowledged or sent back.
pub struct WakeConsumer {
    messages: pull::Stream,
}

/// One delivered message. Dropping it without answering lets the server
/// redeliver it after the consumer's `ack_wait`.
pub struct WakeDelivery {
    msg: jetstream::Message,
    pub wake: WakeMessage,
    /// Which delivery this is: 1 the first time, 2 the second, and so on.
    pub delivered: u64,
}

impl WakeConsumer {
    /// The next message. `Ok(None)` means the stream ended, which a live
    /// consumer does not do: treat it as the connection being gone.
    ///
    /// A message that is not a [`WakeMessage`] is terminated here (it would
    /// never become one) and skipped, and a transient fetch error is returned so
    /// the caller can log it and call again.
    pub async fn next(&mut self) -> Result<Option<WakeDelivery>> {
        loop {
            let Some(item) = self.messages.next().await else { return Ok(None) };
            let msg = item.map_err(ParkError::storage)?;
            match serde_json::from_slice::<WakeMessage>(&msg.payload) {
                Ok(wake) => {
                    let delivered = msg.info().map(|i| i.delivered.max(1) as u64).unwrap_or(1);
                    return Ok(Some(WakeDelivery { msg, wake, delivered }));
                }
                Err(e) => {
                    eprintln!("holon-park: terminating an unreadable PARK_WAKE message: {e}");
                    let _ = msg.ack_with(AckKind::Term).await;
                }
            }
        }
    }
}

impl WakeDelivery {
    /// Finished with: the message leaves the stream.
    pub async fn ack(self) -> Result<()> {
        self.msg.ack().await.map_err(ParkError::storage)
    }

    /// Not finished: send it back, to arrive again after `delay`.
    pub async fn nak(self, delay: Duration) -> Result<()> {
        self.msg.ack_with(AckKind::Nak(Some(delay))).await.map_err(ParkError::storage)
    }
}
