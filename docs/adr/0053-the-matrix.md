
# 0053 — The matrix, and the number it corrected

Status: accepted. Supersedes ADR-0052's per-app figure.

> **Correction ([ADR-0057](0057-the-latency-column-was-arithmetic.md)):** the
> latency columns below are `connections / rps` — Little's law restating the
> throughput column, because the harness was closed-loop. And the rps figures
> are measurements of the STORAGE BACKEND, not the runtime: the same host does
> 30 545 rps with storage out of the way. The comparisons between cells stand;
> the absolute numbers were not measuring what they say.


> **Correction ([ADR-0054](0054-pooling-on-and-the-leak-that-was-not.md)):** the
> drift below is not a leak. A ten-minute cell shows RSS plateau at ~99 MiB and
> then return 23 MiB; the 80-second window ended before the release and saw only
> the climb. Pooling was also run: +21–46% rps at identical idle memory, and it
> is now the default.

## Why a matrix

Every benchmark here moved one axis for fifteen or twenty seconds. That is enough to
compare two throughputs and exactly wrong for the questions being asked by then: what
an idle app costs, whether sharing helps at scale, whether anything leaks. A spike
cannot see drift, and a one-axis run cannot see an interaction.

`comp-matrix` crosses app count × digest sharing × pooling, holds load for as long as
you ask, and samples memory throughout instead of reading it once.

Distinct digests are the **same component with an inert custom section appended** —
behaviour identical, content address different. Using genuinely different components
would have confounded "how much memory does a second module cost" with "these two do
different work".

## What it found, 90s of load per cell, 48 connections

```
 apps   digests │ idle MiB    loaded  per-app │      rps  mean ms │ shared  compiled
    1      same │     47.5      97.7    35.48 │     5866     8.11 │      0         1
    1  distinct │     47.9     100.6    35.94 │     6038     7.96 │      0         1
    8      same │     48.6      90.4     4.58 │     7065     6.80 │      7         1
    8  distinct │     68.8     106.2     7.10 │     6904     6.96 │      0         8
   32      same │     48.4      81.8     1.14 │     7094     6.77 │     31         1
   32  distinct │    112.4     142.7     3.14 │     6611     7.27 │      0        32
```

## The correction

**ADR-0052 published "2.33 MiB per idle app". That number is an artefact.**

It came from one configuration — 16 apps, one digest — and the arithmetic
`(RSS − 12) / apps`. The matrix shows idle RSS on a shared digest is **flat**: 47.5 MiB
at one app, 48.6 at eight, 48.4 at thirty-two. An idle app sharing a digest costs
approximately **nothing**; what I divided was a fixed ~36 MiB of engine and first-module
cost that appears once, whatever the app count.

The real per-app figure depends entirely on the axis I was not varying:

| | marginal cost of one more idle app |
|---|---|
| same digest as one already running | **~0.03 MiB** |
| its own digest | **~2.0 MiB** |

Which also means the saving from sharing is much larger than ADR-0052 claimed, and
grows with the count: **1% at one app, 29% at eight, 57% at thirty-two**. Reported as
27% from a single 16-app pair.

Same mistake shape as ADR-0032's "50×" and ADR-0036's "102k rps": one measurement,
one plausible reading, and the reading attributed to the interesting variable.

## The thing only sustained load could show

```
drift after steady state, 80s window
  apps=1   +16.53 MiB   <- GROWING
  apps=8    +8.91 MiB   <- GROWING
  apps=32   +5.42 MiB   <- GROWING
```

Memory grows under constant arrival rate, after a ten-second settling window, in every
cell. It is not the modules — it happens with one app and one digest. It scales
inversely with app count at roughly constant total throughput, which points at
per-request work rather than per-app state: at one app all 48 connections hit one
instance path.

**This is unexplained and it is not dismissed.** It could be allocator behaviour that
plateaus, a per-request allocation that is not returned, or a cache with no bound —
`SecretCache` and the wRPC clients are both per-store, and per-store means per request
(ADR-0037). Every previous benchmark ran for 15–20 seconds and could not have seen it.

The next run should be one cell for ten minutes rather than six cells for ninety
seconds: growth that plateaus is an allocator, growth that continues is a leak, and
80 seconds cannot tell them apart.

## Throughput, incidentally

5 866–7 094 rps across every cell, and slightly *higher* with more apps than with one —
consistent with a single app's instance path being the bottleneck rather than the host.
Sharing a digest costs nothing in throughput, which is what makes it a free saving
rather than a trade.

## What this does not cover

- **Pooling is off in every cell above.** `--with-pool` doubles the matrix and was not
  run; ADR-0020 measured pooling winning on a different workload and it belongs here.
- **One node.** Every cell is `--nodes 1`, so nothing crosses a machine.
- **One component, 0.4 MB.** The ~2.0 MiB per distinct digest is that component's
  machine code; a larger one costs more and the sharing saving grows with it.
- **`mean ms`, not p50.** The harness keeps a running mean and a max, not a histogram,
  so the latency columns are indicative and the max is a single outlier.
