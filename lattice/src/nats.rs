//! The NATS implementation of all three traits. One struct because one connection
//! serves all of them; callers hold `Arc<dyn Inventory>` and friends, so any ONE
//! trait can get a second implementation without touching the others.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;

use crate::{wire, Artifacts, Command, CommandBus, Entry, Inventory};

pub struct NatsLattice {
    client: async_nats::Client,
    kv: async_nats::jetstream::kv::Store,
    objects: async_nats::jetstream::object_store::ObjectStore,
    lattice: String,
    /// The bucket's REAL `max_age`, which is not necessarily the one this process
    /// asked for — see `connect_bucket`.
    effective_ttl: Duration,
}

/// Split a comma-separated server list, so one flag can name a whole cluster.
///
/// A client given a single address does learn the rest of the cluster from the
/// INFO the server sends, and fails over to them — but only once it has connected
/// to something. A process starting while its one listed server is the one that
/// is down cannot bootstrap at all, which is exactly when it matters (ADR-0067).
/// Everything that dials NATS in this workspace goes through here.
pub fn servers(url: &str) -> Vec<String> {
    url.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl NatsLattice {
    /// `inventory_ttl` becomes the bucket's `max_age`. NATS applies it per BUCKET, not
    /// per entry, so `Inventory::publish`'s `ttl` is honoured at connect time and two
    /// callers asking for different TTLs get whichever connected first.
    pub async fn connect(url: &str, lattice: &str, inventory_ttl: Duration) -> Result<Self> {
        Self::connect_bucket(url, lattice, inventory_ttl, wire::INVENTORY).await
    }

    /// The same thing against a named bucket, so the load signal can reuse this
    /// wholesale instead of growing a fourth trait for one map of counters.
    pub async fn connect_bucket(
        url: &str,
        lattice: &str,
        inventory_ttl: Duration,
        bucket: &str,
    ) -> Result<Self> {
        let urls = servers(url);
        let client = async_nats::connect(urls.clone())
            .await
            .with_context(|| format!("connecting to NATS at {}", urls.join(", ")))?;
        let js = async_nats::jetstream::new(client.clone());

        // Created rather than assumed, so a fresh lattice needs no setup step.
        //
        // AND OPENED rather than insisted upon when it already exists with a different
        // config: host, reconciler and ingress each ask for their own `max_age`.
        // nats-server 2.14.6 REFUSES `create_key_value` there (`stream name already in
        // use with a different configuration`, 10058), so the second process would fail
        // to start with a message naming neither the TTL nor who wanted what, and a
        // changed `--heartbeat-secs` would keep a host from ever joining. So: fall back
        // to the existing bucket and say what differs. The TTL comes back out on
        // `effective_ttl` so a caller sizes its refresh off reality.
        let wanted = async_nats::jetstream::kv::Config {
            bucket: bucket.into(),
            max_age: inventory_ttl,
            ..Default::default()
        };
        let kv = match js.create_key_value(wanted).await {
            Ok(kv) => kv,
            Err(create) => js.get_key_value(bucket).await.with_context(|| {
                format!("opening the inventory bucket `{bucket}` after create said: {create}")
            })?,
        };
        let objects = js
            .create_object_store(async_nats::jetstream::object_store::Config {
                bucket: wire::ARTIFACTS.into(),
                ..Default::default()
            })
            .await
            .context("opening the artifact store")?;

        // WHAT THE BUCKET ACTUALLY SAYS, not what we asked for: a refresh sized against
        // a TTL three times the real one is a poll that mostly sees an empty bucket,
        // which looks like `no app answers`. Read back, reported once when it differs,
        // and carried on the handle. Not fatal: refusing to start would turn a degraded
        // fleet into a dead one.
        let effective_ttl = match kv.status().await {
            Ok(status) => {
                let actual = status.max_age();
                if actual != inventory_ttl {
                    eprintln!(
                        "comp-lattice: bucket `{bucket}` already existed with max_age {}s; \
                         this process asked for {}s and does NOT get it. Whoever created the \
                         bucket first decided. Everything derived from the TTL here now uses \
                         {}s. To change it, update the bucket (`nats kv edit {bucket}`) or \
                         delete it while nothing is running.",
                        actual.as_secs(),
                        inventory_ttl.as_secs(),
                        actual.as_secs(),
                    );
                }
                actual
            }
            // A status read that fails says nothing about the TTL, so the requested
            // value stands rather than a zero being invented — and it is reported,
            // because silently falling back is the failure this whole block is about.
            Err(e) => {
                eprintln!(
                    "comp-lattice: could not read `{bucket}`'s max_age ({e}); assuming the \
                     requested {}s, which may not be what the bucket holds",
                    inventory_ttl.as_secs()
                );
                inventory_ttl
            }
        };

        Ok(Self { client, kv, objects, lattice: lattice.to_string(), effective_ttl })
    }

    /// The inventory bucket's real `max_age`.
    ///
    /// Ask this rather than reusing the value passed to `connect`: they differ
    /// whenever the bucket outlived the config, and anything sized against the wrong
    /// one polls too slowly and reads an empty bucket.
    pub fn effective_ttl(&self) -> Duration {
        self.effective_ttl
    }
}

#[async_trait]
impl Inventory for NatsLattice {
    async fn publish(&self, key: &str, value: Vec<u8>, _ttl: Duration) -> Result<()> {
        // See `connect`: the TTL is the bucket's.
        let value = crate::snapshot::compress(value);
        self.kv.put(key, value.into()).await.context("publishing inventory")?;
        Ok(())
    }

    async fn read_all(&self) -> Result<Vec<Entry>> {
        let mut keys = self.kv.keys().await.context("listing inventory keys")?;
        let mut out = Vec::new();
        while let Some(key) = keys.next().await {
            let key = key.context("reading an inventory key")?;
            // Absent here means it expired between the list and the read. That is
            // the mechanism working, so it is skipped rather than reported.
            if let Some(raw) = self.kv.get(&key).await.context("reading an inventory entry")? {
                out.push(Entry { key, value: crate::snapshot::expand(raw.to_vec()) });
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl CommandBus for NatsLattice {
    async fn serve(&self, node: &str) -> Result<tokio::sync::mpsc::Receiver<Command>> {
        let subject = wire::command_wildcard(&self.lattice, node);
        let mut sub = self
            .client
            .subscribe(subject.clone())
            .await
            .with_context(|| format!("subscribing to {subject}"))?;
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let client = self.client.clone();

        // Translating a subscription into a channel is what keeps the trait free of
        // NATS types. The handler side never sees a `Message`.
        tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                let verb = wire::verb_of(msg.subject.as_str()).to_string();
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let cmd = Command { verb, payload: msg.payload.to_vec(), reply: reply_tx };
                if tx.send(cmd).await.is_err() {
                    break; // the agent is gone
                }
                if let Some(reply_to) = msg.reply {
                    let client = client.clone();
                    tokio::spawn(async move {
                        // A dropped sender means the handler chose not to answer,
                        // which the caller sees as a timeout — no special case.
                        if let Ok(body) = reply_rx.await {
                            let _ = client.publish(reply_to, body.into()).await;
                        }
                    });
                }
            }
        });
        Ok(rx)
    }

    async fn send(
        &self,
        node: &str,
        verb: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let subject = wire::command_subject(&self.lattice, node, verb);
        let reply =
            tokio::time::timeout(timeout, self.client.request(subject.clone(), payload.into()))
                .await
                .map_err(|_| anyhow::anyhow!("no reply within {}s", timeout.as_secs()))?
                // NATS answers `NoResponders` immediately rather than after the
                // timeout, which is the distinction the trait doc asks for.
                .with_context(|| format!("publishing to {subject}"))?;
        Ok(reply.payload.to_vec())
    }
}

#[async_trait]
impl Artifacts for NatsLattice {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        self.objects
            .put(name, &mut bytes.as_slice())
            .await
            .with_context(|| format!("storing {name}"))?;
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Vec<u8>> {
        let mut object =
            self.objects.get(name).await.with_context(|| format!("fetching {name}"))?;
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut object, &mut bytes)
            .await
            .context("reading the artifact")?;
        Ok(bytes)
    }

    async fn has(&self, name: &str) -> bool {
        self.objects.info(name).await.is_ok()
    }
}

/// Watch a KV bucket's keys for changes, as a stream of notifications.
///
/// For `comp-relay`, which needs to know that SOMETHING was published without caring
/// what: `event-bus` bumps `eb.seq.<topic>` per publish, so a watch on `eb.seq.>` is
/// a change feed of bus activity. It lives here because everything that dials NATS
/// goes through this file. The item type is `()` on purpose: a caller reacting to
/// WHICH key changed would be reimplementing `event:bus`'s consumer-group offsets.
pub async fn watch_bucket(
    url: &str,
    bucket: &str,
    subject: &str,
) -> Result<impl futures::Stream<Item = ()>> {
    let urls = servers(url);
    let client = async_nats::connect(urls.clone())
        .await
        .with_context(|| format!("connecting to NATS at {}", urls.join(", ")))?;
    let js = async_nats::jetstream::new(client);
    // `get`, not `create`: the bucket belongs to whatever wrote it. Creating it here
    // would invent an empty one and then report no activity forever, which looks
    // exactly like a quiet system.
    let store = js
        .get_key_value(bucket)
        .await
        .with_context(|| format!("opening the key-value bucket {bucket:?}"))?;
    let watch = store
        .watch(subject)
        .await
        .with_context(|| format!("watching {subject:?} in {bucket:?}"))?;
    Ok(watch.filter_map(|e| async move { e.ok().map(|_| ()) }))
}
