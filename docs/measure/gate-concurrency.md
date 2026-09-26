# Why the gate didn't scale, measured rather than guessed at three times over

A user asked for high parallelism — many agents, each creating a component —
and the first two theories about why `comp-checks` "doesn't scale" were both
wrong. The third was right, and it wasn't the one that looked architectural.

## Theory 1: the compile cache is cold per branch. Wrong.

`comp-goalrun`'s `warm_caches()` (`reconciler/src/bin/goalrun/setup.rs`)
already gives every branch a **shared, persistent** `CARGO_HOME` and
`CARGO_TARGET_DIR` under `~/.cache/comp-goalrun/…` — "paid once, ever," per
its own comment. Measured against this repo's own `record-store`, one distinct
edit per candidate, 12 cores:

| N concurrent builds | shared `target/` | isolated `target/` |
|---:|---:|---:|
| 4 | 12.6 s | — |
| 8 | 7.1 s | ~31 s |
| 16 | 7.7 s | 41.2 s |
| 32 | 7.1 s | 42.7 s |
| 48 | 8.4 s | — |
| 64 | 9.6 s | — |

Flat from N=8 to N=64, zero failures, no lock-contention pathology, on a
12-core machine — nearly 3× core count with no measurable degradation.
Isolated per-branch dirs plateau around 41–43 s regardless of N in this range:
they also handle concurrency fine internally, they just never get to skip the
shared-dependency compile. The difference is entirely cargo's own fingerprint
reuse on a shared `target/`, not avoided lock contention.

Also tested: capping each build's own `--jobs` (worried that K processes ×
cargo's own internal parallelism would oversubscribe past core count).
`--jobs 2`: 8.3 s. `--jobs 1`: 11.6 s. **Uncapped (default): 7.1 s — the
fastest.** Cargo's own fingerprint/lock system on the shared `target/` already
arbitrates the real cost; an external cap only removes headroom.

**Conclusion: this repo's existing compile-cache mechanism needs no fix, and
already scales far past what a "K = core count" worker-pool design would have
assumed necessary.**

## Theory 2: cargo's target-dir lock serialises concurrent branches. Wrong.

The table above answers this too — a shared `target/` under N=64 concurrent
processes shows no serialization signature (wall time stays flat; if the lock
were queuing builds, wall time would grow roughly linearly with N).

## Theory 3: `comp-checks` answers one request at a time. Right.

`reconciler/src/bin/checks.rs` was `for stream in listener.incoming() { … }` —
a single-threaded, fully serial accept loop, with nothing to do with cargo,
caching, or core count. Measured directly against the real binary: 20
concurrent requests, each a fixed `sleep 2`, took **42 s total — exactly
N × duration.** Every check queued strictly behind whatever was already
running, at any concurrency, regardless of how fast an individual check was.

This is the actual bottleneck. Fixed in the commit that added this doc:
each connection now runs on its own thread (`std::thread::scope`), bounded by
`--max-concurrent` (default 256 — a generous safety ceiling, not a throttle;
theory 1's data says the compile step itself doesn't need one). Fixing this
surfaced a real, pre-existing race: `ensure_base`'s `write_tree` stages a new
commit's base at a fixed, commit-keyed path, so two concurrent first-time
requests for the same brand-new commit could corrupt it — closed with a lock
held only for that rare, cheap step, never during the actual check.

Re-measured on the fixed binary: the same 20×2 s workload → **2 s.**
`--max-concurrent 3` against 6 requests → two waves, ~5 s, confirming the
bound is real. 15 concurrent first-time requests for one brand-new commit all
succeeded with no corruption.

## What this means for "a lot of agents, a lot of components"

On one machine, the answer was never "build a queue" — it was "let the
existing mechanisms actually run concurrently." That's done.

**The queue is still the right shape for a second machine** — not to throttle
compiles (they don't need it) but to distribute the request stream across
boxes that each keep their own local warm cache. The template already exists
in this repo, proven at real load: `comp-media`'s `MEDIA_JOBS` JetStream
work-queue stream (`reconciler/src/bin/media.rs`) — a `RetentionPolicy::WorkQueue`
stream, a durable pull consumer, N worker processes each handling one job at a
time. The plan for a second `comp-checks` box, when there is one:

1. A `CHECK_JOBS` JetStream work-queue stream, mirroring `MEDIA_JOBS`'s setup
   exactly.
2. `comp-goalrun` (or whatever drives the swarm) publishes a check request to
   it instead of — or in addition to — calling `comp-checks` over HTTP
   directly.
3. Each worker box runs `comp-checks` with its *own* local `warm_caches()`-style
   persistent cache (unchanged — theory 1 already solved that per-box), pulling
   jobs from the shared consumer instead of listening on a socket.
4. Observability is free from the message lifecycle (unacked = queued/running,
   acked = done, nak'd/redelivered = a worker crashed mid-job) — no separate
   state machine needed, the same way `comp-media`'s queue needs none.

**This is deliberately not built yet.** There is no second machine to build or
test it against, and this repo has already lived through what unverified,
uncalled scaffolding turns into — `GateCaches`/`warm_the_gate_cache` in
`goalrun.rs`'s early history looked exactly like a finished mechanism and
wasn't; only reading `setup.rs` (which really does call it) told the two
apart. Writing the `CHECK_JOBS` version now, with nothing to run it against,
would be the same kind of code: plausible, untested, and someone's future
problem to figure out whether it actually works. When a second box exists,
build this against it for real, the same way every number in this document
came from running the actual binary rather than reasoning about it.
