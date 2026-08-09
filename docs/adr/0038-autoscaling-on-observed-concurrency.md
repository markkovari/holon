# 0038 — Autoscaling on observed concurrency

Status: accepted. Builds on ADR-0037's cold-start measurement.

## The signal, and why it is concurrency rather than rate

`Scale { min, max, target }` on a component, where **`target` is concurrent requests
per replica** — not requests per second.

Concurrency is what the ingress can already observe: it holds an in-flight counter
per backend for least-outstanding balancing, so the number exists and costs nothing
new. Rate needs a clock and a window, and a rate averaged over the wrong window is
how autoscalers oscillate. Concurrency also degrades in the right direction: when
replicas slow down, in-flight rises and more replicas are asked for, which is the
behaviour wanted without modelling latency at all.

The counter had to be added per **host**, not reused per node: several apps share a
node, so the existing counter says how busy the *node* is, not how busy the *app* is.

## The path

```
ingress (per-host in-flight)  ->  comp-load KV  ->  reconciler  ->  plan()  ->  start/stop
```

`comp-load` is a second bucket rather than a key prefix inside `comp-inventory`,
because `read_all` deserialises every entry as a `NodeInventory` and a second shape
in there is a parse error on every pass. The bucket has a 30s TTL, so **an ingress
that dies stops voting** instead of pinning an app at whatever it last published —
the same mechanism that retires a dead node's inventory (ADR-0022).

Several ingresses are summed: two each seeing 5 in flight means the app is carrying
10, not 5.

`desired_replicas` is a pure function beside `place`, so "how many" is testable
without a fleet to put them on. Scale-*down* needs no new machinery — a lower desired
count is a surplus, and the existing asymmetric hysteresis already makes surpluses
wait two settled passes while deficits act immediately.

## Two failure modes that are the whole design

**A missing sample must not read as zero traffic.** The ingress restarting, or the
first pass before any sample exists, would otherwise scale a busy app to `min`
exactly when nobody is watching it. A missing key holds the current count instead.
This has a test named after it.

**An unreadable load bucket must not skip the pass.** Autoscaling is an enhancement
to the diff; manifests with fixed replicas must keep reconciling regardless. Same
rule as ADR-0022's "a failed poll means we know nothing, so we change nothing" —
applied so that a *load* failure is not treated as a *manifest* failure.

## Measured end to end

Unit tests prove arithmetic, not wiring, so: three nodes, `min 1, max 4, target 10`,
idle → 40 concurrent → idle.

```
   0s   0        fleet still starting
   8s   1  #     settled at min
  29s   4  ####  40 concurrent / target 10
  75s   1  #     back to min after the load stopped
```

47 tests pass, six of them new: the arithmetic across the whole range, the missing
sample, a manifest with no `scale` block being inert to load, min-0 reachability, and
a nonsense block (`max` below `min`, `target: 0`) that must not divide by zero.

## What this does not do

**`min: 0` parks an app; it is not yet scale-to-zero.** With no replica placed there
is no route and the ingress answers 503 — nothing brings the app back on a request.
ADR-0037 measured a 33 ms start, so the activation path is affordable; it is simply
not built. The field carries a comment saying so, because a `min: 0` that silently
strands traffic is worse than one that refuses.

Also untouched: the pooling budget. A burst of scale-ups compiles in parallel on one
node against `total_core_instances`, which is ADR-0008's starvation arriving at
runtime rather than at deploy. Nothing checks it yet.
