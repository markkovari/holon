//! One reconciler at a time: two would each count `settle_passes` and both issue stops.
//! The bucket's `max_age` is the lease, so a holder that stops renewing just expires.

use std::time::Duration;

use anyhow::{Context, Result};

/// Separate from inventory so its `max_age` can change independently.
pub const BUCKET: &str = "comp-lease";

pub struct Lease {
    kv: async_nats::jetstream::kv::Store,
    key: String,
    id: String,
    held: Option<u64>,
}

impl Lease {
    /// `ttl` must be well above the reconcile interval, or a leader expires between its own passes.
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

    /// Acquires or renews. Unreachable NATS is `false`: not knowing means changing nothing.
    pub async fn hold(&mut self) -> bool {
        if let Some(rev) = self.held {
            match self.kv.update(&self.key, self.id.clone().into(), rev).await {
                Ok(next) => {
                    self.held = Some(next);
                    return true;
                }
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

    /// Best effort, so a planned restart fails over without waiting out the TTL.
    pub async fn release(&mut self) {
        if let Some(rev) = self.held.take() {
            let _ = self.kv.update(&self.key, "".into(), rev).await;
            let _ = self.kv.delete(&self.key).await;
        }
    }

    pub async fn holder(&self) -> Option<String> {
        let e = self.kv.entry(&self.key).await.ok().flatten()?;
        let who = String::from_utf8(e.value.to_vec()).ok()?;
        (!who.is_empty()).then_some(who)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}
