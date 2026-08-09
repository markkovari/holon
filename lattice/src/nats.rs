//! The NATS implementation of all three traits.
//!
//! It is one struct rather than three because one connection serves all of them
//! and splitting it would only move the plumbing. Nothing outside this file knows
//! that: a caller holds `Arc<dyn Inventory>` and friends, so a second
//! implementation of any ONE of them can be dropped in without touching the other
//! two — which is the point of three traits rather than one.

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
}

impl NatsLattice {
    /// `inventory_ttl` becomes the bucket's `max_age`.
    ///
    /// NATS applies it per BUCKET, not per entry, so `Inventory::publish`'s `ttl`
    /// argument is honoured at connect time rather than per call. Two callers
    /// asking for different TTLs would silently get whichever connected first —
    /// which is fine while the only two are a host and a reconciler configured to
    /// agree, and is exactly the kind of thing a second implementation would do
    /// differently. Said out loud here rather than discovered later.
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
        let client = async_nats::connect(url)
            .await
            .with_context(|| format!("connecting to NATS at {url}"))?;
        let js = async_nats::jetstream::new(client.clone());

        // Created rather than assumed, so a fresh lattice needs no setup step.
        let kv = js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: bucket.into(),
                max_age: inventory_ttl,
                ..Default::default()
            })
            .await
            .context("opening the inventory bucket")?;
        let objects = js
            .create_object_store(async_nats::jetstream::object_store::Config {
                bucket: wire::ARTIFACTS.into(),
                ..Default::default()
            })
            .await
            .context("opening the artifact store")?;

        Ok(Self { client, kv, objects, lattice: lattice.to_string() })
    }
}

#[async_trait]
impl Inventory for NatsLattice {
    async fn publish(&self, key: &str, value: Vec<u8>, _ttl: Duration) -> Result<()> {
        // See `connect`: the TTL is the bucket's.
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
                out.push(Entry { key, value: raw.to_vec() });
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
        let reply = tokio::time::timeout(timeout, self.client.request(subject.clone(), payload.into()))
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
        let mut object = self
            .objects
            .get(name)
            .await
            .with_context(|| format!("fetching {name}"))?;
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
