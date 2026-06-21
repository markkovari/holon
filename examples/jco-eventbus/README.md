# jco-eventbus

Runs the **`event:bus@0.1.0`** WASI component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco).

`event:bus` is an **in-app publish/subscribe bus**. One thing happens
("appointment booked") and several unrelated reactions must run — notify the
owner, write an audit line, update a search projection. Calling each inline
welds the producer to every consumer; a bus inverts it. The producer
`publish`es one event to a **topic**; independent consumers, each in their own
**consumer group**, `poll` at their own pace and `ack` what they handled.
Adding a reaction is a new group, not a producer change.

- `publish(topic, payload)` → a monotonic per-topic event id.
- `poll(topic, group, max)` → unacked events for that group, oldest first
  (does **not** advance — at-least-once).
- `ack(topic, group, ids)` — advance the group's offset past those ids.
- `pending(topic, group)` — the group's unacked backlog.
- `topics()` — every topic that has a log.

**Per-group offsets** = durable fan-out: a slow or newly-added group still sees
past events from its offset, and groups never steal each other's events (unlike
a work queue). Delivery is **pull** (consumers poll) — no push/callback, so it
stays pure WASI; a relay or the app's own loop drives the polling.

Imports `wasi:keyvalue/store` + `atomics` (the atomic per-topic sequence,
mapped to `src/*-shim.js`) and `wasi:clocks` (from jco).

## Run

```bash
npm install
npm test
```

## What the test proves

publish → poll (per-group offset) → ack (advances); fan-out (two groups each
see every event); `pending` backlog; a new group reads from the start;
unacked events re-poll (at-least-once); `topics`.
