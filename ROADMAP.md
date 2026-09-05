# Roadmap — the showcase worklist

> **What this file is.** One section per showcase app: what is built, and what is
> left. It was written when this repository was a library of WASI capability
> components, and for a while that framing was stale — the front page had become an
> agentic engineering loop. It is accurate again: the library and its **delivery**
> are the current focus, and the loop is
> [paused](README.md#the-agentic-loop--paused-and-kept).
>
> For the current state, in order of usefulness:
> [`docs/CURRENT.md`](docs/CURRENT.md) (what runs, measured, and honestly missing),
> [`docs/SELFHOST.md`](docs/SELFHOST.md) (the four ways to deliver an app),
> [`docs/CAPABILITY-GRAPH.md`](docs/CAPABILITY-GRAPH.md) (what is using what, derived
> from the built artifacts), and [`docs/adr/`](docs/adr/) (why any of it is shaped
> this way).

## Where the work is now

**1. The library.** Its size is in [`docs/CAPABILITY-GRAPH.md`](docs/CAPABILITY-GRAPH.md),
derived rather than counted by hand. The per-showcase list below is the live
worklist for this.

**2. Delivery.** Four lanes from one `apps/<name>.toml`, all verified against real
infrastructure. What is left is narrower than what is done:

- [ ] **Tier 2 — many apps per host.** Designed, not built. The blocker is naming,
      not the runtime: every storage component hardcodes `open("default")`, so apps
      sharing one host would share one bucket. Needs `record-store` and its siblings
      to read their bucket from `wasi:config`. Worth doing when RAM pressure says so,
      not for neatness — one crash takes every app on that box
      ([`docs/SELFHOST.md`](docs/SELFHOST.md)).
- [ ] **The remaining eleven contract-only capabilities.** `comp-fswatch` was the
      first to get a daemon under [ADR-0095](docs/adr/0095-what-is-allowed-to-be-native.md);
      `browser-automation`, `container-docker`, `desktop-clipboard`,
      `image-optimizer`, `lan-scanner`, `llm-local`, `mdns-discovery`, `system-cron`,
      `ui-notifier`, `video-ffmpeg` and `vpn-wireguard` still return
      `UNIMPLEMENTED:`. One daemon each, deliberately — `container-docker` and
      `ui-notifier` do not deserve the same blast radius.
- [ ] **`comp:` interfaces on wasmCloud 2.x.** Currently impossible: a release host
      has no host component plugins. Either upstream ships them enabled, or these
      apps stay on the first two lanes. Not a bug to fix here; a constraint to track.

**3. The agentic loop — paused.** Nothing deleted, nothing progressing. The blocker
is that nothing criticises a gate
([`.comp/goals/07-nothing-criticises-a-gate.md`](.comp/goals/07-nothing-criticises-a-gate.md)):
a gate that already passes on the base tree accepts anything. Resume it when that
goal lands; until then more search buys less than better contracts and a way to ship
them.

## What this covers

One section per showcase: what is built, and what is left. The auth-era
evaluation and its Tier 1–5 roadmap lived here until every tier was done; they
are in git history rather than above, because a finished roadmap that still reads
as a plan is the kind of document people act on by mistake.

The native wRPC-to-Golem capability provider headed this list until 2026-08-16 and
is retired with it. Its two hard-won findings are still in
`docs/capabilities/GOLEM.md`; the work brief and the remaining lattice half are in
git history, for the same reason the tiers above are.

*A correction to what this paragraph used to say.* It read "this repository is no
longer connected to wasmCloud", which stopped being true: `holon wadm render` emits
manifests for both wasmCloud 1.x and 2.x, verified against live clusters
([`docs/SELFHOST.md`](docs/SELFHOST.md)). wasmCloud is not on the *runtime* path —
[ADR-0021](docs/adr/0021-there-is-no-kubernetes.md) took it off deliberately and
priced it — but it is a supported delivery target, which is a different claim.

## TODO — next up

### Architecture & Tooling

- [ ] **Deprecate static Markdown catalogs.** The `CAPABILITY-GRAPH.md` and `CATALOG.md` files are static and inevitably go out of sync when code changes. The goal is to move away from generated static `.md` files and rely entirely on dynamically upgrading and querying the Knowledge Graph (via SurrealDB or a UI) to reflect the live state of the component ecosystem.

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
