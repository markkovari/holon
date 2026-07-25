//! The `gate` request batcher as a REAL Golem agent — the exact-coalescing
//! counterpart to the shared-store batcher in `gate-domain`.
//!
//! `#[agent_definition(mount = "/batch/{key}")]` makes **one durable worker per
//! key**. Submits are serialized by the worker, so the buffer never loses or
//! double-counts an item and the flush is inherently atomic — no revision CAS,
//! no re-bucketing under a race (which the shared-store version suffers). Fire N
//! concurrent submits and the worker accounts for EXACTLY N (`stats.total == N`).

use golem_rust::{agent_definition, agent_implementation, endpoint};

/// A per-key batch that coalesces submits and flushes on `MAX_SIZE`.
#[agent_definition(mount = "/batch/{key}")]
pub trait BatchAgent {
    fn new(key: String) -> Self;

    /// Append `item`; auto-flush when the buffer reaches `MAX_SIZE`.
    /// Returns `{size, flushed, total}`.
    #[endpoint(post = "/submit/{item}")]
    fn submit(&mut self, item: String) -> String;

    /// Force-flush a partial batch (the age-timer analog). Returns
    /// `{flushed, flushed_total}`.
    #[endpoint(post = "/flush")]
    fn flush(&mut self) -> String;

    /// Current accounting: `{total, flushed_total, pending}`.
    #[endpoint(get = "/stats")]
    fn stats(&self) -> String;
}

const MAX_SIZE: usize = 4;

struct BatchImpl {
    _key: String,
    items: Vec<String>,
    total: u32,         // items ever submitted
    flushed_total: u32, // items ever flushed
}

/// The "downstream work" per item — an uppercase makes coalescing visible.
fn process(item: &str) -> String {
    item.to_uppercase()
}

impl BatchImpl {
    fn drain(&mut self) -> usize {
        let n = self.items.len();
        // one batched "downstream call" for the whole buffer.
        let _results: Vec<String> = self.items.iter().map(|s| process(s)).collect();
        self.flushed_total += n as u32;
        self.items.clear();
        n
    }
}

#[agent_implementation]
impl BatchAgent for BatchImpl {
    fn new(key: String) -> Self {
        Self { _key: key, items: Vec::new(), total: 0, flushed_total: 0 }
    }

    fn submit(&mut self, item: String) -> String {
        self.items.push(item);
        self.total += 1;
        let flushed = if self.items.len() >= MAX_SIZE {
            self.drain();
            true
        } else {
            false
        };
        format!("{{\"size\":{},\"flushed\":{},\"total\":{}}}", self.items.len(), flushed, self.total)
    }

    fn flush(&mut self) -> String {
        let n = self.drain();
        format!("{{\"flushed\":{},\"flushed_total\":{}}}", n, self.flushed_total)
    }

    fn stats(&self) -> String {
        format!(
            "{{\"total\":{},\"flushed_total\":{},\"pending\":{}}}",
            self.total, self.flushed_total, self.items.len()
        )
    }
}
