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
    effective_ttl: Duration,
}

/// Lists every server so a process can start while one of them is down (ADR-0067).
pub fn servers(url: &str) -> Vec<String> {
    url.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl NatsLattice {
    /// NATS applies `inventory_ttl` per bucket, not per entry; the first creator's value wins.
    pub async fn connect(url: &str, lattice: &str, inventory_ttl: Duration) -> Result<Self> {
        Self::connect_bucket(url, lattice, inventory_ttl, wire::INVENTORY).await
    }

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

        // Open the existing bucket on failure: nats-server refuses a create with a different
        // `max_age` (10058), and host, reconciler and ingress each ask for their own.
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

        // A refresh sized off the requested TTL reads an empty bucket, so use the real one.
        // Not fatal: refusing to start would turn a degraded fleet into a dead one.
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
            // Keep the requested value rather than invent a zero, and say so.
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

    /// Size refreshes off this, not the requested TTL: the bucket may predate the config.
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
            // Absent: it expired between the list and the read, which is not an error.
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
                // `NoResponders` comes back immediately, not after the timeout.
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

/// Yields `()` on purpose: reacting to which key changed would reimplement `event:bus` offsets.
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
    // `get`, not `create`: an invented empty bucket would look like a quiet system forever.
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
