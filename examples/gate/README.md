# gate — durable traffic-shaping gateway (docs/apps/GATE.md)

Three request-shaping patterns — **rate limiting** (token bucket), **throttling**
(GCRA), and **batching** (coalesce + atomic flush) — each keyed by client with
**durable per-key state**, demonstrating the **Golem Cloud durable-worker**
model. The shaping math is the stateless `shaper:limit` component; the durability
is per-key `records:store` state under a revision CAS. See [docs/apps/GATE.md](../../GATE.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/gate.rs            # e2e: token bucket + GCRA + atomic batch flush + a concurrency probe
golem/                   # all three patterns as real Golem agents (one durable worker per key)
```

## Run it on Golem (exact serialization)

`golem/` is the full trio as durable **Golem agents** — one single-writer worker
per key, no CAS. `just gate-golem` deploys to a local Golem and fires concurrent
bursts: rate limit admits **exactly** the capacity, throttle exactly `burst+1`,
the batch accounts for **every** submit (no loss/dup), and a `SubmitAgent`
**durably blocks** on a Golem promise until its batch flushes (real
backpressure) — where the shared-store version below over-admits / re-buckets and
can only poll. Same math, different place for the state. (Reuses the Golem 1.5
binary from `providers/golem-workflow`.)

## Run

```bash
# from the repo root:
just host-gate           # composes the component + builds the UI + serves on :3044
```

Open `http://127.0.0.1:3044`: **Burst ×10** the rate limiter to watch the token
bucket drain to `429`; burst the throttle to watch GCRA space requests out;
submit items to a batch and watch it coalesce and flush. No login — a gateway
keys by a client-supplied API key (the field in the header).

```bash
just e2e-gate            # token bucket + GCRA (deterministic) + batch flush + concurrency probe
# work on the UI live:
cd examples/gate/ui && npm install && npm run dev
```
