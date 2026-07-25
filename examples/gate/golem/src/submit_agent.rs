//! `SubmitAgent` — submits one item to a `BatchAgent` and blocks until the batch
//! flushes, via a Golem promise. It is a **durable** agent keyed by `(key,
//! item)`, so each request is its own worker (concurrent) and can *durably
//! suspend* on the promise — an ephemeral agent can't (it has no oplog to resume
//! from).
//!
//! `POST /submit/{key}/{item}/go` durably suspends until the item's batch runs,
//! then returns the item's result — real backpressure, consuming nothing while
//! suspended, surviving restarts.

use golem_rust::{agent_definition, agent_implementation, blocking_await_promise, create_promise, endpoint};

use crate::batch_agent::BatchAgentClient;

#[agent_definition(mount = "/submit/{key}/{item}")]
pub trait SubmitAgent {
    /// One worker per (batch key, item) — unique per request.
    fn new(key: String, item: String) -> Self;

    /// Register with the batch and block until it flushes; returns the result.
    #[endpoint(post = "/go")]
    async fn go(&mut self) -> String;
}

struct SubmitImpl {
    key: String,
    item: String,
}

#[agent_implementation]
impl SubmitAgent for SubmitImpl {
    fn new(key: String, item: String) -> Self {
        Self { key, item }
    }

    async fn go(&mut self) -> String {
        // We OWN the promise (creator = awaiter); the aggregator (a different
        // worker) completes it on flush — the documented cross-worker pattern.
        let promise = create_promise();
        let mut batch = BatchAgentClient::get(self.key.clone());
        batch.register(self.item.clone(), promise.clone()).await;
        // durably suspend until the batch flushes and completes the promise.
        let bytes = blocking_await_promise(&promise);
        String::from_utf8(bytes).unwrap_or_else(|_| "?".to_string())
    }
}
