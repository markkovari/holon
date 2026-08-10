
# 0054 — Pooling is on by default, and the leak was not a leak

Status: accepted. Closes the two things [ADR-0053](0053-the-matrix.md) left open.

> **Correction ([ADR-0057](0057-the-latency-column-was-arithmetic.md)):** the
> latency columns below are `connections / rps` — Little's law restating the
> throughput column, because the harness was closed-loop. And the rps figures
> are measurements of the STORAGE BACKEND, not the runtime: the same host does
> 30 545 rps with storage out of the way. The comparisons between cells stand;
> the absolute numbers were not measuring what they say.


0053 ended with two admissions: pooling was off in every cell, and 80 seconds
could not tell an allocator settling from a leak. Both were run.

## The ten-minute cell

One app, one digest, 48 connections, RSS sampled every 20 s for ten minutes.

```
   0s   80.6 MiB      200s   98.9 MiB      400s   77.0 MiB
  60s   96.2 MiB      280s   99.9 MiB      480s   76.6 MiB
 120s   98.5 MiB      340s   99.7 MiB      560s   71.2 MiB
 180s   98.0 MiB      360s   77.0 MiB  <- returned
```

It climbs to a plateau at ~99 MiB, holds it for two and a half minutes, and then
**gives 23 MiB back** and stays down for the remaining four minutes. Over the
window: first half +19.7 MiB, second half −22.7 MiB.

That is an allocator holding freed pages and eventually releasing them. **There
is no leak.** 0053's "+16.53 MiB, GROWING" was true and meant nothing — the
80-second window ended before the release, so it only ever saw the climb.

Which is the same mistake shape the last three ADRs have caught, in a new
costume: a window chosen for convenience, a monotone reading inside it, and a
conclusion about the mechanism. The fix here was not cleverness, it was
patience — the run that answered it is the run that took ten minutes.

`comp-matrix --trace` now prints the curve and a verdict, so the next person
does not have to re-derive this from a single delta.

## Pooling, measured on this workload

Six cells, 90 s each, same digest, pooling crossed:

```
   apps  pool │ idle MiB   loaded │      rps   mean ms   max ms
      1   off │     49.3    106.7 │     6033      7.96    113.4
      1    on │     49.3    106.1 │     8812      5.40     38.5
      8   off │     50.7     91.5 │     6965      6.90     50.1
      8    on │     51.6     65.3 │     9028      5.27     49.7
     32   off │     49.3     82.1 │     7214      6.66     46.0
     32    on │     50.4     84.2 │     8723      5.51     30.2
```

**+46% / +30% / +21% throughput, at identical idle memory** — and ADR-0057 later
found this understated: with the storage backend out of the way, which was
masking most of it, pooling is worth **3.1×**. Loaded memory is the same or lower. Pooling
also flattens the drift: at 8 apps the pooled cell *returns* 15.6 MiB where the
on-demand cell grows 9.6 MiB, which is the same page-return behaviour arriving
sooner because slots are reused rather than reallocated.

The gain shrinks as apps rise because the bottleneck moves: at one app all 48
connections share one instance path, and instantiation is the largest thing on
it.

## The change

`--pool` becomes the default; `--no-pool` reproduces the on-demand baseline.
The fleet harness starts hosts pooled, so the tests now run what production
runs — which was not true before, and is the more valuable half of this change.

ADR-0020 measured pooling winning on a different workload and the plan called
for making it unconditional. It stayed opt-in because "the naive baseline" was
the honest default while nothing had measured it *here*. Something has now.

## Bounds

- The pooling limits are fixed at 1000 component instances / 10 000 core
  instances / 10 000 memories / 64 MiB per linear memory. Those are a guess
  sized for this machine, not a fleet calculation. A node that exceeds them
  fails to instantiate rather than degrading, so they need to become flags
  derived from placement before a node is packed near them.
- One node, one 0.4 MB component, no distinct-digest cells — `--with-pool`
  crossed with `--only same` only. Pooling's interaction with many distinct
  modules is unmeasured.
- The ten-minute run is one configuration. A plateau at one app is not proof of
  a plateau at thirty-two; the drift column says those grow more slowly, which
  is consistent but not the same thing as measured.
