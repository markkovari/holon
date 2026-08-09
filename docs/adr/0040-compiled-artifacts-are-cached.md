# 0040 — Compiled artifacts are cached

Status: accepted. Acts on the measurement in ADR-0037.

## What was wrong

ADR-0037 measured a 33 ms cold start and found **94% of it was `wasmtime` compiling**
bytes this node had already compiled. `fetch_artifact` cached the raw `.wasm`, and
`start` called `Component::from_file` — so every start recompiled: on scale-up, on
every re-placement after a node died (ADR-0035), and on every reboot.

## The change

Compile once per artifact per node, keyed by the digest, into
`<state-dir>/cache/<digest>.cwasm`. On start: `deserialize_file` if the file is
there, compile and write it if not.

| | cold (nothing cached) | warm (compiled artifact cached) |
|---|---|---|
| **total** | **35.2 ms** | **0.43 ms** |
| fetch | 1.48 ms | 0.02 ms |
| build | 33.7 ms | 0.30 ms |
| link | 0.12 ms | 0.12 ms |

**81× faster, 98.8% of the start removed.** Phase timings moved to microseconds for
this run — in milliseconds the warm number rounds to `0 ms`, which reads as "free"
and would have been reported as a 100% saving.

## Why this is safe, and the one line that makes it so

`Component::deserialize_file` is `unsafe` because it trusts its input completely: it
maps machine code straight in. A `.cwasm` from an untrusted source is arbitrary code
execution, full stop.

What makes it acceptable here is provenance, and nothing else: the file is **written
by this process**, into a **host-private directory**, named for the **digest whose
bytes were verified before compiling** (ADR-0024). Nothing off the wire is ever
deserialised. That constraint is the whole safety argument, so it is stated at the
`unsafe` block rather than in a commit message.

Two failure modes are handled because both will happen:

- **A cache written by a different `wasmtime` build**, after an upgrade. It must not
  be fatal — it is a cache. The error is caught, the file dropped, and the artifact
  recompiled.
- **A torn write**, from two starts of the same digest racing. Written to a temp file
  in the same directory and `rename`d, so a reader never sees a half-written file.

The corrupt case has a check in `bench/coldstart/corrupt.py`: fill the cache with a
sentence, start the instance, and assert it serves again, logs that it dropped the
bad cache, and rewrites a good one. All three pass. It is a real test of the branch
that could brick a node, which is worth more than a unit test of the happy path.

## What this does not change

- **The first start on a node still pays 35 ms.** This removes repeats, not the
  original compile. A pre-warm pass that compiles an artifact before it is needed is
  a separate thing and is not built.
- **The cache is never evicted.** One `.cwasm` per digest per node, forever. That is
  fine while a node holds tens of apps and wrong at thousands.
  `// ponytail:` unbounded; add an LRU when a node's cache directory is big enough to
  notice.
- **Nothing pre-compiles across the fleet.** Every node compiles each artifact once
  for itself; the object store holds `.wasm`, not `.cwasm`, because a compiled
  artifact is only valid for a matching `wasmtime` and CPU, and shipping one over the
  wire would be exactly the untrusted-bytes case the safety argument above rules out.
