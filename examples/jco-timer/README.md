# jco-timer

Runs the **`sched:timer@0.1.0`** WASI component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco) — no wasmCloud host required.

`sched:timer` is a **durable timer / scheduler**: a store of future jobs that a
relay polls. The naive "do this later" is a cron line plus a script that scans a
table; the portable version is this component. It owns the *when* — eligibility,
recurrence, leasing — and a relay owns the *what* (read `due` jobs, do the work,
`ack` the one-shots). The work itself (notify, an HTTP call, an outbox enqueue)
stays out of scope, so the component is pure WASI and composes with any sink.

- `scheduleAt(key, runAt, payload)` — fire **once** at `runAt`, then gone.
- `scheduleEvery(key, period, firstRunAt, payload)` — fire **every** `period`
  seconds; each `due` advances the next run-at.
- `due(now, max, leaseSeconds)` — claim eligible jobs. One-shots are **leased**
  (won't re-return until the lease lapses → crash-safe at-least-once); recurring
  jobs advance their run-at to the next future slot, so a long outage fires
  **once**, not a backlog burst.
- `ack(key)` / `cancel(key)` / `peek(key)` / `listJobs(max)`.

`key` is app-chosen and unique: re-scheduling the same key **replaces** the
prior job, so a "nightly-sweep" scheduled on every boot never duplicates.

The component imports `wasi:keyvalue/store`, `wasi:keyvalue/atomics`, and
`wasi:clocks/wall-clock`. jco satisfies the clock automatically; the two
key-value imports are mapped to local host shims:

- `src/keyvalue-shim.js` — in-memory bucket store (the `buckets` Map).
- `src/atomics-shim.js` — `increment` over the **same** store.

Both shims are **swappable**: point the store at redis or NATS and the component
is unchanged — it neither knows nor cares.

## Run

```bash
npm install
npm test          # transpiles timer.wasm -> gen/, then runs the node:test suite
```

## What the test proves

- one-shot `scheduleAt` → `due` (leased) → `ack` (gone);
- a lapsed lease re-arms a one-shot (a crashed relay's job becomes due again);
- recurring `scheduleEvery` advances run-at by the period and **catches up**
  (a 10-hour outage fires once, not ten times);
- same-key re-schedule replaces (idempotent);
- `cancel` / `not-found` / `invalid-period` error surface.
