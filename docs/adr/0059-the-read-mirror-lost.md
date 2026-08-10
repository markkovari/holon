
# 0059 — The read mirror lost, 2.3×

Status: rejected. The code is not in the tree.

Rejected ADRs are rarer than they should be. This one is written because the
idea is a good idea, it will be proposed again, and the reason it fails here is
not obvious from the outside.

## The idea

[ADR-0057](0057-the-latency-column-was-arithmetic.md) found that the request
path is dominated by JetStream round trips: `memory` does 30 545 rps where
`nats` does 9 755. The obvious response is to keep an in-memory mirror of the
NATS KV bucket on each host — NATS provides exactly this primitive, `watch()` —
so reads are served locally and only writes cross the wire.

Built behind `--kv-mirror`: lazily populated, write-through, kept fresh by a
watch per bucket, with `atomics::increment` deliberately excluded. That exclusion
is not an optimisation but a correctness requirement — increment is a
compare-and-swap against a revision the mirror does not track, so a cached read
there is a lost update rather than a stale one.

## The measurement

```
 mirror off   10278 rps   p50  4.54   p99  8.01   p99.9 13.36
 mirror on     4504 rps   p50 10.31   p99 20.25   p99.9 27.47
```

**2.3× slower.**

## Why, and why it is structural

The watch delivers every write to every node, including the node that made it.
The benchmark component writes on every request, so at ten thousand requests a
second the mirror processes ten thousand watch messages a second, each taking a
write lock that the readers are contending for.

A read cache whose upkeep scales with the **fleet's write rate** is exactly
backwards for a write-heavy workload. This is not a tuning problem: a sharded
lock or a lock-free map would reduce the contention and would not change the
fact that the work exists and grows with every writer in the fleet.

## What would have to be true for it to win

The workload would have to be read-heavy — and nothing in this repo is. The one
component the benchmarks drive does `get` → compute → `set` per request, which is
the least cacheable shape there is. `wasi:keyvalue` does distinguish the safe
case (`store::get`) from the unsafe one (`atomics::increment`), so the split is
expressible; there is simply no workload here on the safe side of it.

Which is the deciding argument. A flag whose win case is hypothetical and whose
loss case is measured at 2.3× is speculative complexity with a benchmark
attached. It was reverted rather than shipped off by default.

## What to do instead, if this comes up again

- **Measure the op mix of a real application first.** The entire question turns
  on the read/write ratio, and the platform has never seen one.
- **Single-owner routing is the version that cannot lose.** If exactly one
  replica owns a key, its state is authoritative in memory with no coordination
  at all — the Durable Objects shape. The ingress already routes by `Host`
  header; routing by key is the extension. The costs are real and different:
  failover needs a persistence story, and a guest cannot tell the router which
  key it is about to touch before the call.
- **CRDTs only work where the host understands the value.** `wasi:keyvalue` is
  opaque bytes, so the host cannot merge — except in `atomics`, where the
  operation *is* the merge and a per-replica counter summed on read needs no
  coordination. Exact as a total, approximate at the instant of reading: right
  for metrics, wrong for a limit that must not be exceeded.

The patch is recoverable from this ADR's commit history if a read-heavy workload
ever arrives to justify it.
