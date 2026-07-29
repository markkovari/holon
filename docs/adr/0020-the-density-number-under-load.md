# ADR-0020 — The same density number, under load: free throughput, 3.2× memory, better tail

- **Status:** accepted
- **Date:** 2026-07-29
- **Completes:** [ADR-0019](0019-the-density-number.md), which measured idle floors and said plainly that the performance claim was "not even attempted"

## Context

ADR-0019 established that an extra component inside a host costs 2.3 Mi against 70 Mi for
a component in its own pod, and then listed the obvious objection against itself: every
figure was taken at `1m` CPU. A density claim measured idle invites exactly one question —
*and under traffic?* — and the honest answer was that nobody had asked.

## The measurement

Same setup as ADR-0019 (`kv-probe`, `poolSize: 8`, wash 2.5.2), load generated **inside the
cluster** with `oha` so the laptop's network path is not what gets measured. One node, 10
cores, 20 GiB.

### Does packing cost throughput? No.

50 connections, 15 s, against one endpoint:

| | rps | p50 | p95 | p99 | pod memory |
|---|---|---|---|---|---|
| 1 component, its own pod | 16 925 | 2.79 ms | 4.85 ms | 6.15 ms | 235 Mi |
| **4 components, one pod** | **16 650** | 2.81 ms | 5.02 ms | 6.46 ms | 212 Mi |
| 4 components + a keyvalue round-trip | 15 020 | 3.17 ms | 5.30 ms | 6.59 ms | 212 Mi |

Adding three components to a host costs **1.6 % throughput and 0.02 ms at p50** — noise. A
round-trip to the app's own private NATS costs about **10 % throughput and 0.4 ms**, which
is the real price of the storage isolation ADR-0014 bought.

### The density question, under saturation

Same total traffic — 200 connections — arranged two ways on the same node:

| | rps | p50 | **p99** | CPU | **memory** |
|---|---|---|---|---|---|
| **one pod, 4 components** | **20 041** | 9.73 ms | **16.52 ms** | 5 078 m | **257 Mi** |
| four pods, 1 component each | 20 064 | 9.22 ms | 25.96 ms | 5 172 m | 820 Mi |

Throughput identical (0.1 %). CPU identical (1.8 %). Memory **3.2× lower**. And the tail is
**36 % better packed** — which was not predicted: four independent hosts each pooling and
scheduling separately produce more variance than one host multiplexing over one pool.

Both shapes were CPU-saturated at 5 of 10 cores plus the load generator, so this is the
regime a density claim has to survive, not a comfortable one.

### Sustained: no leak, but memory is sticky

100 connections for 60 s against the keyvalue path: **16 486 rps, p50 5.87 ms, p99 10.77 ms,
989 157 responses, 97 errors** (the errors are in-flight requests at cutoff, one per
connection, in every run).

```
t+10s  cpu    2m  mem 225Mi     <- pool already warm from earlier runs
t+20s  cpu 2391m  mem 253Mi
t+30s  cpu 4084m  mem 253Mi
t+40s  cpu 4084m  mem 253Mi
t+50s  cpu 4171m  mem 249Mi
t+60s  cpu 4250m  mem 250Mi
idle+30s  cpu 4m  mem 233Mi     <- does NOT fall back to the 86Mi idle floor
```

Flat across a million requests: **no leak**. But it does not give the memory back — a pod
that has served traffic settles around **233 Mi**, not the **86 Mi** it started at.

## Decision

**The density claim survives load, and the numbers to quote publicly are these, not
ADR-0019's.**

- **Packing components is free in throughput and CPU, and 3.2× cheaper in memory.** The
  headline stands and gets stronger: it is not a memory-versus-speed trade, because the
  speed is identical and the tail is better.
- **Quote the loaded footprint, never the idle one.** ADR-0019's `70 Mi` floor and `2.3 Mi`
  slope are *cold-start* numbers. Capacity planning wants ~**233 Mi per host pod that has
  served traffic**, and the extrapolations in ADR-0019 (296 Mi for 100 components) describe
  a host that has never taken a request. The **ratio** holds; the absolute floor roughly
  triples.
- **Price the private bus honestly: ~10 % throughput.** That is what per-app storage
  isolation costs on the request path. Cheap for what it buys, and it should be a published
  number rather than a discovered one.
- **~4 000 rps per core** for a trivial component, so ~16–20 k rps per host pod on this
  hardware. Enough that a per-app host is not a throughput bottleneck for most apps —
  which was the other unspoken worry about pod-per-app.

## Consequences

- **ADR-0019's self-criticism is discharged**, and its idle figures are now labelled as
  cold-start rather than operating cost. Both documents are needed: 0019 for the shape of
  the cost, this one for the magnitude.
- **The better tail is an argument nobody made for wasm.** Fewer pods means fewer
  independent schedulers, pools and network paths on one request's critical path. It
  suggests packing is the *safer* choice under load, not merely the cheaper one — on one
  node, at one component size.
- **A quota that counted idle memory would be wrong by 3×.** Nothing meters memory yet;
  when it does, it must meter the loaded figure.
- **What is still not measured**: more than one node; a large component (`kv-probe` is
  75 KB — a 1 MB component will have a different slope and a different warm-pool cost);
  behaviour at `maxInvocations` (nothing was shed in any run); mixed workloads where one
  component in a packed host is hostile or hot; and anything longer than a minute. The
  claim now holds for one minute on one node; "steady state" is still a word this repo has
  not earned.
- The load generator ran in-cluster and consumed some of the same 10 cores, so absolute rps
  is understated for both shapes equally. Comparisons are sound; the ceiling is not.

## Alternatives

- **Publish ADR-0019's idle numbers alone.** Rejected: they are 3× optimistic on memory and
  say nothing about throughput, which is the first question any reader has.
- **Benchmark with a realistic component instead of `kv-probe`.** Better, and not done: a
  trivial component isolates *platform* overhead, which is what was in question. A realistic
  one measures the app. Both are worth having; this ADR is honest about which it is.
- **Test on multiple nodes.** The right next step for a throughput claim, and irrelevant to
  the memory ratio, which is per-pod arithmetic.
