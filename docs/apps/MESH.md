# mesh — resilient upstream calls (the breaker trips, the app stays up)

Every service that calls another service needs the same three behaviours, and
almost every codebase re-invents them inline, differently, at each call site:

- **retry** with exponential backoff + jitter — ride out a blip;
- a **circuit breaker** — stop dialling an upstream that is already down, so the
  caller fails *fast* instead of piling threads onto a corpse;
- an **SLO** — "too slow" is a failure, not a success that happens to be late.

`mesh` is those three in front of a **deliberately flaky upstream** you can drive
from the UI: make it 500, make it slow, make it unreachable, and watch the
circuit go `closed → open → half-open → closed`.

The algorithms are a new **`resilience:breaker`** component. The upstream hop is a
**real outgoing HTTP request** through `proxy:route` — which is what makes the
demo honest: when the breaker is open, the upstream's own hit counter *stops
moving*.

![The mesh app: a shield badge reads “closed — calls flow, failures are counted” with counters for calls / ok / failed / shed / trips. A healthy call logs one 200 attempt; a 300ms response against a 100ms SLO logs “failed · slo breach: 304ms > 100ms” despite its 200. Hammering the failing upstream flips the badge red to “open — tripped, requests are refused here, the upstream is not dialled at all”, and the next calls log “shed — circuit open · upstream not called” while the shed counter climbs and calls stops moving. The countdown reaches “cooldown over — next call probes”, one healthy call gets through, and the badge returns to green “closed”. A live recording of the running React app.](../media/mesh.gif)

## The component (why `resilience:breaker`)

Same shape as `shaper:limit` (GATE.md): the algorithms are **stateless pure
functions**, and the caller owns the state.

```wit
admit:   func(state: circuit, now-ms: u64, pol: policy) -> tuple<admission, circuit>
observe: func(state: circuit, now-ms: u64, pol: policy, ok: bool) -> circuit
backoff: func(attempt: u32, pol: retry-policy, seed: u64) -> option<u32>
```

Two calls wrap the work — `admit` before, `observe` after — and each returns the
circuit to persist. No clock, no storage, no randomness inside: `now-ms` and the
jitter `seed` are arguments. That is what lets the *same* breaker guard a
`records:store` record here, a per-tenant Golem worker there, or a plain in-memory
map in a unit test — and it is why the state machine has **exhaustive unit tests**
(`cargo test -p resilience`) instead of a sleep-and-hope integration suite.

```text
               failures >= failure-threshold
       CLOSED ────────────────────────────────► OPEN
         ▲                                       │ cooldown open-ms elapsed
         │ successes >= success-threshold         ▼
         └──────────────── HALF-OPEN ◄────── (admit up to half-open-probes)
                              │
                              └── one probe fails ──► OPEN
```

`failure-threshold` counts **consecutive** failures; `window-ms` bounds how long a
partial streak is remembered, so an upstream that fails once an hour never trips.
`backoff` grows `base-ms` by `factor-pct` per attempt, caps at `max-ms`, and with
`jitter` picks uniformly in `[base, delay]` (decorrelated jitter) so a herd of
retriers doesn't re-synchronise into a second outage.

## One guarded call, in order

`mesh-domain` exports `wasi:http` and imports only contracts: `resilience:breaker`
(the math), `records:store` (the durable circuit), `proxy:route` (the real hop).

1. **`admit`** — if the circuit is OPEN the request is shed right here: `503`,
   `shed: true`, and the upstream is never dialled.
2. **forward** — a real outgoing HTTP request to the path the caller asked for.
3. **judge** — a `5xx`, an unreachable upstream, or a response slower than
   `slo_ms` is a FAILURE. `observe` feeds that back into the circuit.
4. **retry** — wait `backoff(attempt)` and go again, *re-checking `admit` each
   time*, because our own retries may be what trips the breaker.

Every circuit mutation is a revision-guarded `records:store` update, so concurrent
callers converge on one circuit instead of clobbering each other's counters.

One deliberate asymmetry: an **unreachable** upstream is an upstream failure and
trips the breaker, but a **missing route** is *our* misconfiguration — it answers
`502` and is not counted. A config bug must not trip a breaker.

## The flaky upstream (nothing is simulated)

`examples/mesh/src/bin/flaky.rs` — ~100 lines, std only. The **caller** decides how
it misbehaves, per request, so the demo and the e2e are deterministic (no random
failure percentage to flake on):

| request | behaviour |
| --- | --- |
| `/hit` | 200 |
| `/hit?fail=1` | 500, always |
| `/hit?fail_n=2&id=x` | 500 for the first 2 requests tagged `x`, then 200 — a blip a retry rides out |
| `/hit?delay=400` | 200, 400ms late — trips an SLO |
| `/count?id=x` | how many `/hit` requests were tagged `x` |
| *kill the process* | a real connect-refused |

`/count` is the proof: after the breaker trips, the e2e keeps calling and asserts
the upstream's hit count **does not move**. A breaker that "works" but still opens
a socket has done nothing.

## Run it

```bash
just host-mesh     # composes, builds the SPA, serves on :3050 (+ flaky upstream on :3051)
# hit "Hammer it" to trip the breaker, then keep clicking: 503 shed, and the
# upstream stops seeing requests. Wait out the countdown for the half-open probe.

just e2e-mesh          # the whole ladder against the real upstream (see below)
cargo test -p resilience   # the state machine, exhaustively, no host
```

`just mesh-upstream` runs the flaky server alone if you want it to survive host
restarts.

The e2e (`examples/mesh/tests/mesh.rs`) proves: a healthy call takes one attempt;
a two-request blip is ridden out by retries (and `total_ms` shows the backoff
really slept); `failure_threshold` failures trip the breaker and the upstream's hit
counter freezes; after `open_ms` a probe closes it again; a 300ms response with
`slo_ms: 100` is a **failure despite its 200**; an unreachable upstream trips the
breaker and a missing route does not.

## What this does *not* do (and where it goes)

- **The SLO is a judgement, not a timeout.** We cannot cancel an in-flight
  `wasi:http` request, so a slow response is *counted* as a failure after it
  arrives — the connection is not freed early. A real timeout needs
  `wasi:io/poll` with a deadline plumbed through `proxy:route`.
- **Consecutive failures, not an error ratio.** One counter instead of a bucket
  ring. Identical behaviour for the failure mode that matters (an upstream that is
  actually down); an upstream stuck at 30% errors forever will *not* trip it.
  Upgrade path: a bucket ring in the same record — or `stats:describe` if that
  component ever lands.
- **No bulkhead.** Bounding in-flight calls needs a counter incremented before and
  decremented after, which leaks on a crash — and admission control is already
  `gate:app`'s job (GATE.md). Skipped on purpose.
- **The CAS is not exact serialization.** Under a thundering herd the
  revision-guarded read-modify-write degrades toward last-writer-wins, so a trip
  can be briefly missed — the same honest caveat as `gate` (GATE.md), and the same
  fix: one durable single-writer worker per circuit on Golem, where `admit` and
  `observe` serialize by construction.

## Rungs left

- **Real timeouts** — a deadline through `proxy:route` that abandons the request.
- **Hedged requests** — fire a second attempt at `p95` and take the first answer;
  needs concurrency the single-threaded request model doesn't have today.
- **Fallbacks** — a cached or static response when the circuit is open, so shed
  load degrades instead of erroring (`cache:store` is already a component).
- **The breaker on Golem** — one durable worker per circuit, as `gate` did
  (`just gate-golem`), for exact serialization under a burst.
- **Wire it into an existing app** — `eshop-gateway` and `event-pusher` both call
  upstreams through `proxy:route` with no breaker at all.
