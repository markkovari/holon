# Roadmap — the showcase worklist

> **What this file is, and is not.** It was written when this repository was a
> library of WASI capability components, and its opening section still evaluates
> it as one: an auth + RBAC contract with a reference implementation. That is no
> longer what the repository is *for* — Holon is an agentic engineering loop built
> out of that library (see [`README.md`](README.md)) — but the per-showcase
> worklist below is still live and still accurate about what is done.
>
> For the current state, in order of usefulness:
> [`docs/CURRENT.md`](docs/CURRENT.md) (what runs, measured, and honestly
> missing), [`docs/SCENARIOS.md`](docs/SCENARIOS.md) (how a run succeeds and every
> way it fails), [`.comp/goals/`](.comp/goals/) (the worklist a person writes), and
> [`docs/adr/`](docs/adr/) (why any of it is shaped this way).
>
> Kept rather than deleted for the same reason `docs/PLATFORM.md` is: its reasoning is
> the record of how the substrate got here.

## What this covers

One section per showcase: what is built, and what is left. The auth-era
evaluation and its Tier 1–5 roadmap lived here until every tier was done; they
are in git history rather than above, because a finished roadmap that still reads
as a plan is the kind of document people act on by mistake.

The native wRPC-to-Golem capability provider headed this list until 2026-08-16 and
is retired with it: **this repository is no longer connected to wasmCloud.** The
provider was live-verified against a real Golem worker and its two hard-won
findings are still in `docs/capabilities/GOLEM.md`; the work brief and the
remaining lattice half are in git history, for the same reason the tiers above
are.

## TODO — next up

### Helpdesk (docs/apps/HELPDESK.md)

- [ ] Rungs 2–7: multi-tenant + API keys + quotas, event-bus fan-out +
      notifications + signed webhooks, `mail-parse`, SLA timers + search,
      billing rollup, AI drafts. Rung 1 is done (`components/helpdesk-domain`,
      `examples/jco-helpdesk`, `just host-helpdesk` on the native host + NATS).

### Conduit / RealWorld (docs/apps/CONDUIT.md) — done

- [x] The full RealWorld ("Conduit") spec as one `conduit-domain` component +
      `auth-guard` + `record-store` + `slug`, run on the native Rust host.
      **Passes the official RealWorld Hurl conformance suite 100% (13/13 files,
      154 requests)** — `just conformance-conduit`; suite vendored + pinned under
      `examples/conduit/conformance`. Rust e2e (`just e2e-conduit`) + app-path
      bench (`bench/CONDUIT-BENCH.md`, round 13). This is the first showcase
      validated against an *external, objective* test suite rather than our own.
- [ ] Optional: password rotation + email rename in auth-guard (the two flagged
      conformance caveats), a `?search` extension (`search-index`), and a stock
      RealWorld frontend SPA served from `--static-dir`.

### Saga / durable orchestration (docs/apps/SAGA.md) — done

- [x] A durable **trip-booking saga** (`saga-domain`) — flight → hotel → car with
      **compensation** on failure — composed over `fsm-workflow` + `record-store`
      + `idempotency-guard` + `event-bus` + `sched:timer` + `id-generate`. The
      first showcase exercising **compensation + durable, resumable execution**:
      a flaky leg retries then gives up + compensates; `pump` advances one
      persisted step at a time; and the saga **survives a host kill and resumes**
      on NATS (`just durable-saga` → PASS). `just e2e-saga` (commit + all
      compensation/retry paths) + app-path bench (`bench/SAGA-BENCH.md`).
- [ ] Extract a generic `saga:orchestrator` contract (arbitrary step +
      compensation definitions), and land the Golem wRPC provider so a leg can be
      a real durable Golem worker (the roadmap item above).

### Realtime / pulse (docs/apps/REALTIME.md) — done

- [x] A realtime chat room (`pulse-domain`) — the first showcase in a new
      *class*: a **sustained connection with server push** rather than
      request/response. `GET /api/rooms/{room}/events` holds the HTTP response
      open and streams new messages as **Server-Sent-Events** `data:` frames
      (real push on wasip2 — the host streams the body while the guest loops;
      no WebSocket, no wasip3 async). Composed over `record-store` (log + cursor)
      + `event-bus` (fan-out spine) + `id-generate`, with a two-pane browser SPA
      (native `EventSource`) + presence. `just e2e-pulse` (a held-open reader
      gets a separately-posted message) + bench: one broadcast → **150/150
      concurrent SSE connections** (`bench/PULSE-BENCH.md`).
- [ ] Multi-host fan-out: replace per-stream polling with `event-bus` +
      `event-pusher` push, so connections can spread across hosts.
