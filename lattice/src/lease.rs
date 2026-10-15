//! One reconciler at a time, decided by the store rather than by configuration.
//!
//! A lease rather than sharding: a steady pass over 1000 nodes and 10 000 apps is
//! 46 ms and the expensive pass after a fleet change is 1.23 s (ADR-0056), so
//! throughput is not the problem. The gap is that the reconciler has no standby: if
//! it dies, nothing converges. Running two is not safe either: scale-DOWN waits for
//! `settle_passes` consecutive passes counted in each process's `Hysteresis`, so two
//! loops disagree about when the cooldown elapsed and both issue stops.
//!
//! The mechanism is a JetStream KV bucket whose `max_age` IS the lease: a holder that
//! stops renewing has its key expire, with no lease-breaking protocol.
//!
//! * acquire — `create`, which lands only if the key is absent
//! * renew — `update` guarded by the revision we hold
//!
//! Losing the race and losing the lease are the same path: stop being the leader,
//! keep asking.

use std::time::Duration;

use anyhow::{Context, Result};

/// The lease bucket. Separate from inventory because its `max_age` means
/// something different and must be able to change independently.
pub const BUCKET: &str = "comp-lease";

pub struct Lease {
    kv: async_nats::jetstream::kv::Store,
    key: String,
    id: String,
    /// The revision we last wrote, when we hold it. `None` means we do not.
    held: Option<u64>,
}

impl Lease {
    /// `ttl` is how long a leader survives without renewing — so failover takes
    /// up to `ttl` plus one interval. It must be comfortably longer than the
    /// reconciler's interval, or a leader would expire between its own passes.
    pub async fn connect(url: &str, lattice: &str, ttl: Duration, id: &str) -> Result<Self> {
        let urls = crate::nats::servers(url);
        let client = async_nats::connect(urls.clone())
            .await
            .with_context(|| format!("connecting to NATS at {}", urls.join(", ")))?;
        let js = async_nats::jetstream::new(client);
        let kv = js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: BUCKET.into(),
                max_age: ttl,
                history: 1,
                ..Default::default()
            })
            .await
            .context("opening the lease bucket")?;
        Ok(Self { kv, key: format!("leader.{lattice}"), id: id.to_string(), held: None })
    }

    /// Are we the leader right now? Acquires or renews as a side effect.
    ///
    /// Returns `false` rather than an error when NATS cannot be reached, and that
    /// is deliberate: the loop's oldest rule is that not knowing means changing
    /// nothing (a failed inventory poll is not an empty fleet). A reconciler that
    /// cannot see the lease cannot see the inventory either, so standing down is
    /// both the safe answer and the honest one.
    pub async fn hold(&mut self) -> bool {
        if let Some(rev) = self.held {
            match self.kv.update(&self.key, self.id.clone().into(), rev).await {
                Ok(next) => {
                    self.held = Some(next);
                    return true;
                }
                // Either it expired and someone else took it, or we were slow
                // enough that our own key aged out. Both mean: not us, for now.
                Err(_) => self.held = None,
            }
        }
        match self.kv.create(&self.key, self.id.clone().into()).await {
            Ok(rev) => {
                self.held = Some(rev);
                true
            }
            Err(_) => false,
        }
    }

    /// Give it up on the way out, so a planned restart fails over immediately
    /// instead of waiting for the TTL. Best effort — the TTL is what makes this
    /// optional rather than load-bearing.
    pub async fn release(&mut self) {
        if let Some(rev) = self.held.take() {
            let _ = self.kv.update(&self.key, "".into(), rev).await;
            let _ = self.kv.delete(&self.key).await;
        }
    }

    /// Who the store says holds it. For logging what a standby is waiting for.
    pub async fn holder(&self) -> Option<String> {
        let e = self.kv.entry(&self.key).await.ok().flatten()?;
        let who = String::from_utf8(e.value.to_vec()).ok()?;
        (!who.is_empty()).then_some(who)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}
