
# 0057 — The latency column was arithmetic, and the rps column was NATS

Status: accepted. Corrects the latency figures in
[ADR-0053](0053-the-matrix.md) and [ADR-0054](0054-pooling-on-and-the-leak-that-was-not.md),
and reinterprets their throughput.

Two things were wrong with every performance number this project had published.
Neither was a bug in the platform.

## The latency column could not have been informative

Every cell in ADR-0053 and 0054 held 48 connections open, each worker sending
again as soon as its own response came back. That pins concurrency, and Little's
law then fixes mean latency at `concurrency / throughput`. Reported against
computed, to two decimals:

```
   rps   reported   48/rps
  6033      7.96     7.96
  8812      5.40     5.45
  6965      6.90     6.89
  7094      6.77     6.77
  6611      7.27     7.26
```

Twelve cells, all of them. The column was a restatement of the column beside it.
It could not be wrong and it could not be informative.

ADR-0036 already knew this and used `oha --latency-correction`; the matrix
regressed to a closed loop when it was written, and nobody checked because the
numbers looked plausible.

`--rate` now offers a fixed arrival rate and times each request from when it was
**due**, not from when it was sent — so a generator falling behind reports its
own backlog instead of hiding it, which is the whole of coordinated omission.
The mode is printed in the header, so a closed-loop run says so out loud.

## The throughput column was measuring the storage backend

Nobody had ever profiled the request path. Twenty seconds of `sample` under load
put a third of guest-call time in `NatsKv::store_for` and the rest of it in the
keyvalue path. The benchmark component does a `get`, computes, and does a
`set_many` per request — and on the NATS backend each of those is a JetStream
round trip.

Same load, same host, only the backend changed:

```
 backend │    rps   p50 ms  p99 ms  p99.9
  memory │  30545     1.55    2.87   3.56
    nats │   9755     4.75    8.68  13.25
  sqlite │   7721     5.86   13.90  17.57
```

**Every rps this project has published was a measurement of the storage
backend.** With storage out of the way the host does 30 545 rps on this machine,
three times what any previous number suggested.

Which moves two earlier conclusions:

- **Pooling is worth 3.1×, not 46%.** 10 748 → 30 033 rps on the memory backend.
  ADR-0054 measured 46% because the JetStream round trip was masking the
  instantiation cost pooling removes. The decision it made was right; the size of
  the win was understated by a factor of six.
- **The ingress hop costs 21%**, 30 033 → 23 556. I had guessed it might be half
  the request. It is not, and the guess would have sent the next week in the
  wrong direction.

`store_for`'s mutex and its per-call `String` are gone — after the profile
pointed there. It is worth 4.6% (9 755 → 10 202), not the third the samples
suggested: those samples were the round trip inlined into the lookup. Kept
anyway, because a global lock on every keyvalue operation is a hazard that grows
with node density. But no amount of lock-fiddling touches a network round trip,
and that is the cost.

## Two harness bugs found on the way

Both produced numbers that read as platform behaviour:

- `pool_max_idle_per_host(1)` on the load generator caused ~50 000 connection
  failures per run. With them counted as refusals it looked like the platform
  shedding 15% of traffic; removing it took the same cell from 7 011 to 9 823 rps
  with zero errors.
- With no `--rate`, the schedule collapsed to zero gap and "latency from due"
  became "time since the run started" — 22-second p50s.

So transport failures are now reported separately from 4xx/5xx, **with the first
error's text**. A count alone cannot distinguish "the platform refused" from
"the load generator ran out of sockets", and this project has now been fooled by
that class of thing five times ([0032](0032-cross-node-invocation-and-what-the-hop-costs.md),
[0036](0036-open-loop-stress-and-a-correction.md),
[0052](0052-one-copy-per-digest.md), [0053](0053-the-matrix.md), here).

## The same on a Raspberry Pi 5

Because "one machine's number" is not a platform claim, and because the
distribution story only means anything if a small node is a useful node. Pi 5,
4 cores, 8 GB, 12 workers instead of 48 — a softer load for a smaller box.

```
                        │   Mac (10 cores)  │   Pi 5 (4 cores)  │  ratio
  memory, direct        │   30 545 rps      │    4 709 rps      │   6.5x
  nats, direct          │   10 202 rps      │    1 394 rps      │   7.3x
  memory, via ingress   │   23 556 rps      │    2 662 rps      │   8.9x
```

The Pi is a consistent 6–9× behind, which is roughly the core count times the
per-core difference and contains no surprise. What matters is that the SHAPE is
identical on both: the storage backend dominates (3.4× between memory and NATS
on the Pi, 3.1× on the Mac), and sharing a digest saves the same fraction —
33% at 8 apps and 61% at 32, against 29% and 57% on the Mac.

## Density, which is the number a small node is actually for

**200 apps on one Pi 5:**

```
  apps  │ idle MiB   per-app │    rps  p50 ms  p99 ms  p99.9 │ shared  compiled
   200  │     45.3      0.17 │   4427    2.61    5.08   7.26 │    199         1
```

45 MiB for two hundred apps, and **throughput identical to running one** (4 427
against 4 366). The marginal idle app on a shared digest costs about 0.017 MiB
here — 199 of the 200 got the machine code that the first one compiled.

Which reframes what the throughput number is for. A node this size is not
interesting because of its rps; it is interesting because two hundred mostly-idle
tenants fit on it for the price of the runtime, and the rps is then shared by
whichever of them is busy. The 8 GB of RAM is nowhere near the binding
constraint at this density — something else will be, and nobody knows what yet.

## Bounds

- One component, one node, two machines. 30 545 rps is this Mac's number for
  this 0.4 MB component, not a platform constant — the Pi does 4 709 for the
  same work.
- The Pi's ingress cell shed 3 412 requests where the Mac's shed none. Attributed
  here to four cores and a fixed `max_inflight`, and that was **wrong** — see
  [ADR-0060](0060-the-ingress-forgot-what-it-was-told.md), which found the Mac
  sheds too and traced it to an activation the ingress never published.
- The 200-app cell is 200 apps IDLE but for the traffic to one of them — and that
  was a limitation of the load generator, not a choice. ADR-0060 fixes it: 200
  apps all busy do 31 972 rps.
- The memory backend is node-local, so 30 545 rps is not available to a spread
  stateful app. It is the runtime's ceiling with storage removed, which is what
  makes it the right number for judging runtime changes — and the wrong number to
  quote as a product claim.
- ADR-0039's "3.6× wasmCloud" compared the same component on both, so the
  comparison stands even though both sides were storage-bound. The absolute
  figures on each side were not measuring what they said.
