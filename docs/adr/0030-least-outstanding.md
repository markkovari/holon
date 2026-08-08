# ADR-0030 — Least-outstanding, because round robin collapsed on a real fleet

- **Status:** accepted
- **Date:** 2026-08-08
- **Corrects:** [ADR-0029](0029-one-address-in-front-of-n-replicas.md), which shipped round robin and said the slow-backend case had not been provoked

## Context

ADR-0029 chose round robin and marked it, honestly, as untested against a backend that is
*up but slow* — a round robin's one weak case. It also carried a `ponytail:` comment saying
to revisit "when one replica is measurably slower than its peers and the even split
actually hurts".

It is, and it does. No simulation was needed: a Raspberry Pi 5 alongside a MacBook is
already a heterogeneous fleet.

## The measurement

Two Mac nodes, two Pi nodes, one app, `replicas: 5`, 60 concurrent connections for 20
seconds through one ingress. Same fleet, same load, only the algorithm changed.

| | round robin | least-outstanding |
|---|---|---|
| throughput | 710 rps | **6,984 rps** |
| p50 | 8.9 ms | **4.0 ms** |
| p95 | 342 ms | **6.8 ms** |
| p99 | 749 ms | **10.2 ms** |
| success | 100% | 100% |

**A 9.8× throughput difference and a 73× better tail**, from a scheduling rule. Round robin
was not slightly worse; it was catastrophic.

The per-node probe says why. Measured directly rather than assumed:

```
mac-1   5112 rps
pi-1      78 rps
```

An even split sent a quarter of all traffic to nodes 65× slower, and every client queued
behind them. Least-outstanding reached 6,984 rps — *higher than either Mac node alone* —
because it used both fast nodes properly and barely touched the slow ones.

## Why the Pi is 65× slower, which matters more than the number

Not the hardware. On one machine, with everything else identical and only the store
changed:

```
kv=sqlite   9484 rps   (local file)
kv=nats     5209 rps   (loopback NATS)
```

Loopback NATS already costs ~45%. The Pi pays that **over the LAN**: a network round trip
per `get` and per `set`, several per request. Slower hardware is a minor term.

So the real finding is not "the Pi is slow". It is: **a node whose store is remote is
dramatically slower than one whose store is local** — which is exactly what a
geographically spread fleet looks like, and exactly what ADR-0027 requires for a spread
stateful app. Those two ADRs pull against each other, and this one is what makes the
combination usable: the balancer notices the cost without anyone measuring or configuring
it.

## Decision

**`least-outstanding` is the default.** Each request goes to whichever replica currently
has the fewest in flight. It equals round robin when every backend is equally fast, and
degrades gracefully when one is not — a slow node accumulates in-flight requests and stops
being chosen, with no latency measurement, no health check and no configured weight.
Round robin stays selectable so the two can be compared on one fleet, which is how the
table above exists.

Three details that are each a bug if got wrong, and each have a test:

- **Rotate before sorting.** On a fleet that is keeping up, every counter is zero, and a
  stable sort over equal keys hands every request to the same node forever. Rotating first
  means ties still spread.
- **Counters live outside the routing table.** The table is replaced wholesale on every
  inventory refresh; counters replaced with it would reset every few seconds, which is
  exactly often enough to hide the imbalance they exist to correct.
- **The in-flight guard decrements on drop.** A leaked increment retires a healthy backend
  permanently — worse than any imbalance it was meant to fix.

The retry path reuses the same ranking rather than a second rule, so the fallback is simply
the next-best backend. A separate retry rule would be one more thing to get wrong, firing
exactly when things are already going badly.

## What is still not known

- **Behaviour when a backend is slow but the fleet is saturated.** Both algorithms were
  measured with headroom. With every node at capacity there is no good choice to make, and
  least-outstanding's advantage should shrink to nothing.
- **Whether the ~45% loopback-NATS cost is inherent** or an artefact of doing one `get`
  plus one `set` per request with no pipelining. The rate limiter is a read-modify-write;
  batching or the atomic path might close most of it.
- **Nothing here is measured across a WAN.** LAN latency to the Pi already dominates; a
  real multi-region deployment is worse, and that is the case ADR-0027's shared-store
  requirement makes unavoidable.
