//! An in-process implementation of all three traits.
//!
//! Not a toy. It exists for two reasons and both are load-bearing:
//!
//! 1. **An interface with exactly one implementation has never been shown to be an
//!    interface.** Everything NATS-shaped that leaked into the trait signatures
//!    shows up here as something awkward or impossible to write, which is the only
//!    honest test of whether the abstraction is real.
//! 2. **It lets the reconcile loop be tested without a broker.** A test that needs
//!    `nats-server` running is a test that gets skipped.
//!
//! Entry expiry is implemented with a real clock, because it is the property the
//! whole design leans on — a node that stops publishing has to disappear on its
//! own, or `plan()` will keep placing work on a corpse.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::{Artifacts, Command, CommandBus, Entry, Inventory};

type Handlers = Arc<Mutex<HashMap<String, tokio::sync::mpsc::Sender<Command>>>>;

#[derive(Clone, Default)]
pub struct MemoryLattice {
    entries: Arc<Mutex<HashMap<String, (Vec<u8>, Instant)>>>,
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    handlers: Handlers,
}

impl MemoryLattice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop an entry as though its node had gone quiet, without waiting out a TTL.
    /// The failover path is the one worth testing and nobody should wait 15s to.
    pub fn expire(&self, key: &str) {
        self.entries.lock().unwrap().remove(key);
    }
}

#[async_trait]
impl Inventory for MemoryLattice {
    async fn publish(&self, key: &str, value: Vec<u8>, ttl: Duration) -> Result<()> {
        // Per-entry TTL, which NATS cannot do (it is per bucket). Worth noting: the
        // trait signature allows something the current production impl approximates,
        // and that is the abstraction being honest rather than NATS-shaped.
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_string(), (value, Instant::now() + ttl));
        Ok(())
    }

    async fn read_all(&self) -> Result<Vec<Entry>> {
        let now = Instant::now();
        let mut g = self.entries.lock().unwrap();
        g.retain(|_, (_, deadline)| *deadline > now);
        Ok(g.iter().map(|(k, (v, _))| Entry { key: k.clone(), value: v.clone() }).collect())
    }
}

#[async_trait]
impl CommandBus for MemoryLattice {
    async fn serve(&self, node: &str) -> Result<tokio::sync::mpsc::Receiver<Command>> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        self.handlers.lock().unwrap().insert(node.to_string(), tx);
        Ok(rx)
    }

    async fn send(
        &self,
        node: &str,
        verb: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let tx = self.handlers.lock().unwrap().get(node).cloned();
        // The "nobody is listening" case the trait doc asks to be prompt about.
        let Some(tx) = tx else { bail!("no responders for node {node}") };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send(Command { verb: verb.to_string(), payload, reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("node {node} stopped listening"))?;
        tokio::time::timeout(timeout, reply_rx)
            .await
            .map_err(|_| anyhow::anyhow!("no reply within {}s", timeout.as_secs()))?
            .map_err(|_| anyhow::anyhow!("node {node} dropped the command without answering"))
    }
}

#[async_trait]
impl Artifacts for MemoryLattice {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        self.objects.lock().unwrap().insert(name.to_string(), bytes);
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no artifact {name}"))
    }

    async fn has(&self, name: &str) -> bool {
        self.objects.lock().unwrap().contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_entry_expires_without_anyone_deleting_it() {
        // THE property. A departed node disappears on its own; that is what
        // replaced ADR-0016's orphan reaping, and an implementation that cannot do
        // it would make `plan()` keep placing work on a corpse.
        let l = MemoryLattice::new();
        l.publish("box-a", b"alive".to_vec(), Duration::from_millis(40)).await.unwrap();
        assert_eq!(l.read_all().await.unwrap().len(), 1);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(l.read_all().await.unwrap().is_empty(), "the entry should have expired");
    }

    #[tokio::test]
    async fn a_command_is_delivered_and_acked() {
        let l = MemoryLattice::new();
        let mut rx = l.serve("box-a").await.unwrap();
        let worker = tokio::spawn(async move {
            let cmd = rx.recv().await.expect("a command");
            assert_eq!(cmd.verb, "start");
            assert_eq!(cmd.payload, b"{}".to_vec());
            let _ = cmd.reply.send(b"{\"ok\":true}".to_vec());
        });
        let reply = l
            .send("box-a", "start", b"{}".to_vec(), Duration::from_secs(1))
            .await
            .expect("ack");
        assert_eq!(reply, b"{\"ok\":true}".to_vec());
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn sending_to_a_node_nobody_serves_fails_promptly() {
        // Promptly, not after the timeout: "nothing is running there" and "that
        // node is slow" have different fixes.
        let l = MemoryLattice::new();
        let started = Instant::now();
        let err = l
            .send("ghost", "start", vec![], Duration::from_secs(30))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no responders"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1), "it waited out the timeout");
    }

    #[tokio::test]
    async fn a_dropped_reply_reads_as_a_timeout() {
        // A handler that decides not to answer must not hang the sender forever.
        let l = MemoryLattice::new();
        let mut rx = l.serve("box-a").await.unwrap();
        tokio::spawn(async move {
            let cmd = rx.recv().await.unwrap();
            drop(cmd.reply);
        });
        assert!(l.send("box-a", "start", vec![], Duration::from_millis(200)).await.is_err());
    }

    #[tokio::test]
    async fn artifacts_are_content_addressed_and_idempotent() {
        let l = MemoryLattice::new();
        assert!(!l.has("sha256:abc").await);
        l.put("sha256:abc", b"wasm".to_vec()).await.unwrap();
        assert!(l.has("sha256:abc").await);
        assert_eq!(l.get("sha256:abc").await.unwrap(), b"wasm".to_vec());
        // A re-put of the same name is free rather than an error, which is what
        // makes a retried distribution pass cost nothing.
        l.put("sha256:abc", b"wasm".to_vec()).await.unwrap();
        assert!(l.get("sha256:missing").await.is_err());
    }
}
