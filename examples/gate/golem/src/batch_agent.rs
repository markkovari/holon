//! The `gate` request batcher as REAL Golem agents — exact coalescing AND true
//! promise backpressure, the two things the shared-store `gate-domain` can't do.
//!
//! `BatchAgent` (`mount = "/batch/{key}"`) is one durable aggregator worker per
//! key. Submits are serialized by the worker, so the buffer never loses or
//! double-counts an item and the flush is inherently atomic — no CAS, no
//! re-bucketing under a race.
//!
//! Backpressure: `register` creates a **Golem promise** per item and returns it;
//! the caller (an ephemeral `SubmitAgent`, below) awaits it and is *durably
//! suspended* — consuming nothing — until the batch flushes and the aggregator
//! completes every pending promise with its result. Real "block until my batch
//! runs", not polling.

use golem_rust::{agent_definition, agent_implementation, complete_promise, endpoint, PromiseId};

/// A per-key batch that coalesces submits and flushes on `MAX_SIZE`.
#[agent_definition(mount = "/batch/{key}")]
pub trait BatchAgent {
    fn new(key: String) -> Self;

    /// Fire-and-forget append; auto-flush at `MAX_SIZE`. Returns
    /// `{size, flushed, total}`.
    #[endpoint(post = "/submit/{item}")]
    fn submit(&mut self, item: String) -> String;

    /// Append `item` bound to the caller's `promise`; the promise is completed
    /// with the item's result when this batch flushes (backpressure). Called via
    /// RPC by a SubmitAgent (which owns + awaits the promise).
    fn register(&mut self, item: String, promise: PromiseId);

    /// Force-flush a partial batch (the age-timer analog), completing any pending
    /// promises. Returns `{flushed, flushed_total}`.
    #[endpoint(post = "/flush")]
    fn flush(&mut self) -> String;

    /// Current accounting: `{total, flushed_total, pending}`.
    #[endpoint(get = "/stats")]
    fn stats(&self) -> String;
}

const MAX_SIZE: usize = 4;

struct BatchImpl {
    _key: String,
    /// buffered items; `Some(promise)` for a caller awaiting backpressure.
    pending: Vec<(String, Option<PromiseId>)>,
    total: u32,
    flushed_total: u32,
}

/// The "downstream work" per item — an uppercase makes coalescing visible.
fn process(item: &str) -> String {
    item.to_uppercase()
}

impl BatchImpl {
    /// One batched "downstream call" for the whole buffer; complete any waiter
    /// promises with their results. Returns the number flushed.
    fn drain(&mut self) -> usize {
        let n = self.pending.len();
        for (item, promise) in std::mem::take(&mut self.pending) {
            let out = process(&item);
            if let Some(pid) = promise {
                complete_promise(&pid, out.as_bytes());
            }
        }
        self.flushed_total += n as u32;
        n
    }
}

#[agent_implementation]
impl BatchAgent for BatchImpl {
    fn new(key: String) -> Self {
        Self { _key: key, pending: Vec::new(), total: 0, flushed_total: 0 }
    }

    fn submit(&mut self, item: String) -> String {
        self.pending.push((item, None));
        self.total += 1;
        let flushed = self.pending.len() >= MAX_SIZE;
        if flushed {
            self.drain();
        }
        format!("{{\"size\":{},\"flushed\":{},\"total\":{}}}", self.pending.len(), flushed, self.total)
    }

    fn register(&mut self, item: String, promise: PromiseId) {
        self.pending.push((item, Some(promise)));
        self.total += 1;
        if self.pending.len() >= MAX_SIZE {
            self.drain(); // completes the callers' promises; their awaits return
        }
    }

    fn flush(&mut self) -> String {
        let n = self.drain();
        format!("{{\"flushed\":{},\"flushed_total\":{}}}", n, self.flushed_total)
    }

    fn stats(&self) -> String {
        format!(
            "{{\"total\":{},\"flushed_total\":{},\"pending\":{}}}",
            self.total, self.flushed_total, self.pending.len()
        )
    }
}
