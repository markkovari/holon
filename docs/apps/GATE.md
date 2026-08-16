# gate — a durable traffic-shaping gateway (the Golem worker patterns)

Three request-shaping patterns every API gateway needs — **rate limiting**,
**throttling**, and **batching** — each keyed by client and backed by **durable
per-key state**. The app answers a specific question: *can you do rate limiting /
throttling / batching with Golem Cloud?* Yes — and this shows **why** the durable
**worker** model fits them so well, by building each pattern here and mapping it
to its Golem equivalent.

The shaping **math** is the stateless **`shaper:limit`** component (token bucket +
GCRA); the **durability** is this domain's per-key records in **`records:store`**.
The frontend is a **React + shadcn/ui** SPA with three live panels — burst the
rate limiter and watch the token bucket drain to `429`; burst the throttle and
watch GCRA space requests out; submit items and watch a batch coalesce and flush.

![The gate app: three panels. Rate limit (token bucket, capacity 5) — a burst shows five 200s draining a token meter then 429s with a retry-after; Throttle (GCRA, 4/s) — a burst admits two then 429s spaced ~230 ms apart; Batch (coalesce, max 4) — four sample items accumulate then flush together, each mapped to an uppercased result with a “flushed” badge. A footer notes that on Golem Cloud each key is a single-threaded durable worker.](../media/gate.gif)

## The one primitive: a durable worker per key

A Golem **worker** is a durable, single-threaded actor addressed by name (say
`ratelimiter-{apiKey}`). Three properties do all the work:

- **Serialized invocations** — one at a time, so it's a built-in serialization
  point (no locks, no CAS).
- **Durable state** — the worker's memory is persisted via the oplog and replayed
  after a crash. No external store for the counter.
- **Self-scheduling + promises** — a worker can schedule a future invocation and
  hand callers a promise to await.

That's virtual actors (Orleans / Dapr) or Durable Objects, with durable execution
underneath. Here, **each per-key record updated under a revision compare-and-set
stands in for that worker** — same idea, approximated over a shared store.

## The three patterns → their Golem mapping

| pattern | here (on `comp-host`) | on Golem Cloud |
|---|---|---|
| **rate limit** | token bucket in a per-key record; `shaper::token-bucket` computes refill+spend | a `ratelimiter-{key}` worker holding the bucket in durable memory |
| **throttle** | GCRA over one per-key `tat` timestamp; `shaper::gcra` | a worker holding the TAT; deny returns an exact `retry-after` |
| **batch** | a per-key buffer record; one CAS update appends and (on trip) flushes | an aggregator worker; the flush is an **atomic region**; callers hold **promises** |

**Rate limit** — a token bucket refills to `capacity` at `refill/s` and spends a
cost per request; a full bucket lets a burst through, then it's `429` with the
time-to-refill.

**Throttle** — GCRA (Generic Cell Rate Algorithm) smooths to a steady rate with a
burst budget using a *single* "theoretical arrival time" timestamp — no queue, no
background timer. Denied requests get the exact `retry-after` to space them out.

**Batch** — submits append to an open per-key batch; when it hits `max_size` or
ages past `max_age_ms`, the tripping submit **flushes** it (one downstream call
for all items) and returns per-item results. The append-and-flush is a single
revision-guarded update — the **atomic region** that guarantees a crash can't
flush twice or lose buffered items.

## Why this app argues *for* Golem (the honest part)

Sequentially, the shapers are exact — the e2e pins the token bucket to
`[200,200,200,429,429]`, GCRA's burst-then-spacing, and the batch's coalesced
flush. **Under concurrency they are not**, and that's the lesson:

Serializing concurrent writers to shared per-key state needs a **compare-and-swap**,
and `wasi:keyvalue@0.2.0-draft` has **none** (only atomic `increment`). So
`records:store`'s revision check is best-effort read-modify-write; under a
thundering herd on one key it degrades toward last-writer-wins and **over-admits**
— a real limiter breach (the e2e fires 24 concurrent requests at a capacity-10
key and logs how many slip through). A **Golem worker closes exactly this gap**:
one single-threaded durable actor per key serializes writes with *no CAS at all*,
making the limit exact — and it survives restarts for free. That property — exact
per-entity serialization + durable state without a coordination dance — is the
reason rate limiting / throttling / batching are such a natural fit for durable
workers.

## Run it on Golem — exact serialization (done)

The claim isn't hypothetical: `examples/gate/golem` is the same limiter as a
**real Golem agent**, and `just gate-golem` deploys it to a local Golem and
proves the difference.

All three patterns are durable-worker versions — no store, no CAS, because the
worker *is* the serialization point. State lives in the worker's own memory
(durable via Golem's oplog, replayed after a restart) and every invocation is
serialized because a worker runs one at a time:

```rust
#[agent_definition(mount = "/gate/{key}")]     // one durable worker per key
pub trait GateAgent {
    fn new(key: String) -> Self;
    #[endpoint(post = "/take")]     fn take(&mut self) -> String;      // token bucket
    #[endpoint(post = "/throttle")] fn throttle(&mut self) -> String;  // GCRA
    #[endpoint(post = "/reset")]    fn reset(&mut self) -> String;
}

#[agent_definition(mount = "/batch/{key}")]    // an aggregator worker per key
pub trait BatchAgent {
    fn new(key: String) -> Self;
    #[endpoint(post = "/submit/{item}")] fn submit(&mut self, item: String) -> String;
    fn register(&mut self, item: String, promise: PromiseId);  // RPC: bind a waiter's promise
    #[endpoint(post = "/flush")]         fn flush(&mut self) -> String;
    #[endpoint(get = "/stats")]          fn stats(&self) -> String;
}

// A durable, per-(key,item) submitter that BLOCKS until its batch runs:
#[agent_definition(mount = "/submit/{key}/{item}")]
pub trait SubmitAgent {
    fn new(key: String, item: String) -> Self;
    #[endpoint(post = "/go")]
    async fn go(&mut self) -> String;   // create a promise, register it, await it
}
```

Under the **same concurrent bursts that made `gate-domain` over-admit / re-bucket**,
every pattern is **exact** — `just gate-golem`:

```
rate limit  (token bucket, capacity 10): 24 concurrent takes
  trial 0..2: 10/24 admitted -> EXACT (10)        # shared-store: ~16/24
throttle    (GCRA 5/s, burst 2): 12 concurrent
  trial 0..2: 3/12 admitted -> EXACT (3)
batch       (coalesce, max 4): 10 concurrent submits
  trial 0..2: total=10, flushed=10, pending=0 -> EXACT (no lost/dup)
backpressure(promise): 3 submits BLOCK, the 4th fills the batch -> all 4 release
  3 blocked while batch not full: True; 4th released all 4 with results: True
```

**Backpressure** is the piece a shared store can't do at all: `SubmitAgent.go`
creates a Golem **promise**, hands it to the aggregator, and **durably suspends**
(consuming nothing, surviving restarts) until the batch flushes and completes it.
The owner-awaits / other-worker-completes split is the documented promise pattern.
Three submits block; the fourth fills the batch and all four wake together with
their results — real "wait for my batch to run", not polling.

Same algorithms, same load; the difference is *where the state lives*. Moving
`gate` onto Golem was flipping `records:store` for the worker's own durable
memory — the shaping math is unchanged. That's the payoff of the durable-worker
model for stateful shaping: exact per-entity serialization + durability, with no
coordination dance. (`golem-bridge` is how a composed component reaches such a
worker over HTTP.)

## The data model

- **buckets** — `{key, tokens, updated_ms}` (token-bucket state per key).
- **gcra** — `{key, tat}` (the theoretical arrival time per key).
- **batches** — `{key, items, created_ms, max_size, max_age_ms, flushed, results}`.

## Run it

```bash
just host-gate    # composes the component, builds the React UI, serves on :3044
# burst the rate limiter / throttle; submit items to a batch and watch it flush.
just e2e-gate     # token bucket + GCRA (deterministic) + atomic batch flush +
                  # a concurrency probe that documents the shared-store CAS breach
```

To run the **Golem** version (deploys to a local Golem, proves exact serialization):

```bash
just gate-golem   # all three patterns, exact under concurrent bursts
```

## Rungs left

- **Exact limiter without Golem** — a fixed-window counter over `wasi:keyvalue`
  atomic `increment` is exact (but less flexible than a bucket); a nice
  side-by-side with the CAS version.
- **Age-based flush on Golem** — self-schedule the aggregator (Golem's scheduled
  invocation) to flush a partial batch after `max_age_ms`, completing waiters —
  today a partial batch waits for `flush` or the next fill.
