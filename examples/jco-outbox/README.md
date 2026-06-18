# jco-outbox

Runs the **`outbox:dispatch@0.1.0`** WASI component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco) — no wasmCloud host required.

`outbox:dispatch` is a **transactional outbox**: a durable buffer for
reliable, at-least-once event delivery. Producers `enqueue` events (with an
optional delay); a delivery loop `claim`s a leased batch, then `ack`s the ones
it shipped or `fail`s the ones it didn't. Failed events are rescheduled with
backoff until `max-attempts` is exhausted, after which they land in the
dead-letter queue (`deadLetters`) where they can be inspected and `replay`ed.

The component imports `wasi:keyvalue/store`, `wasi:keyvalue/atomics`,
`wasi:clocks`, `wasi:random`, and `wasi:config`. jco satisfies the standard
clocks/random imports automatically; the three non-standard imports are mapped
to local host shims:

- `src/keyvalue-shim.js` — in-memory bucket store (the `buckets` Map).
- `src/atomics-shim.js` — `increment` over the **same** store (attempt counters).
- `src/config-shim.js` — `max-attempts` / `base-backoff` knobs.

All three shims are **swappable**: point the store/atomics at redis or NATS and
the config at your real runtime config, and the component is unchanged — it
neither knows nor cares.

## Run

```bash
npm install
npm test          # transpiles outbox.wasm -> gen/, then runs the node:test suite
```

Tune the delivery policy via env before testing:

```bash
MAX_ATTEMPTS=3 BASE_BACKOFF=2 npm test
```

## Composition

`outbox:dispatch` is the reliable source; it composes naturally with sink
components like **`notify:dispatch`** (fan-out to channels) and
**`webhook:ingest`** (HTTP delivery) — drain claimed batches into a sink, `ack`
on success, `fail` on error, and let the outbox handle retries and dead-letters.
