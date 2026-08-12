# 0064 — The cross-node cost of the read cache, measured

Status: accepted, as a measurement. Discharges the gap
[ADR-0063](0063-a-ttl-is-cheaper-than-coherence.md) named and did not close, and
narrows what that flag is unsafe for.

## What 0063 left open

`--kv-cache-ms` has no coherence protocol, so a write on one node is invisible on
another until the entry expires. ADR-0063 said so plainly and then proved only the
single-node half — on one node the cache invalidates its own writes, so
read-your-own-writes holds by construction and the conformance suite passing was
never evidence about a fleet.

## The probe that failed, and why it is the more useful result

The obvious instrument was the rate limiter. Two replicas must share one budget
(ADR-0027), so a stale read should let more requests through than the limit
allows. Two nodes, capacity 50, 200 attempts:

```
no cache             50 allowed, 150 refused
--kv-cache-ms 1000   50 allowed, 150 refused
```

Identical. Not a broken test — an answer to a different question.

`/api/ratelimit` is a **read-modify-write**: every request writes the key it just
read. Each node therefore invalidates its own entry on every request it handles
and never holds one long enough to serve it stale. The cache is invisible to that
workload.

> **A workload that writes what it reads cannot be made stale by this cache.**

Which is a pleasant irony: the shape ADR-0059 rejected the read mirror on —
`gate-domain`, one write per request — is precisely the shape this cache is safe
for. It is also the shape it does nothing for.

## The exposure, isolated

Staleness needs a key one node **writes** and another only **reads**. Batching is
that pair: `POST /api/batch/submit` grows a batch, `GET /api/batch/{id}` only
reads it. Node 1 writes, node 2 reads, both addressed directly so no balancer
decides the experiment.

```
                     node 2 saw      then      (truth: 1, then 2)
no cache                  1            2
--kv-cache-ms 1000        1            1       <- stale
after the TTL elapses     -            2       <- and it heals
```

Node 2 must read once **before** node 1's second write. Without that the key is
merely cold there and the next read is a miss — a cache cannot be stale about
something it has never seen, and a probe skipping that step would acquit the cache
for the wrong reason.

## So what the flag is actually unsafe for

| the key is… | exposure |
|---|---|
| read and written by the same node (read-modify-write) | none, by construction |
| written by one node, read by another | **stale for up to the TTL** |
| never written | none |

The middle row is most of what an application does — the user record read
everywhere and written at sign-up, the article read on every list and written
once. ADR-0062 measured exactly that shape at 264 reads per write, which is why
the cache is worth so much and why this row is the price.

It remains bounded: the staleness expires with the TTL, on its own, with no
protocol. That is the trade ADR-0063 made, now with both sides measured rather
than one side measured and the other asserted.

## What this still does not measure

**Two writers.** Both tests here have one writing node. Two nodes writing the same
key concurrently, each holding its own cached read, is a lost-update shape rather
than a stale-read shape, and nothing above touches it. `increment` is excluded
from the cache for that reason, but a get/set read-modify-write across two nodes
is not — and `/api/ratelimit` only escaped it because each node also invalidates
locally, which does not make concurrent nodes safe, only individually consistent.

That measurement is [ADR-0065](0065-the-cache-defeats-the-revision-guard.md), and
it found a lost update: the cache does not weaken `record-store`'s revision guard,
it bypasses it, because that guard is a read-compare-write over the same cached
`wasi:keyvalue`.

## Repro

`cargo nextest run --release --test staleness` — two nodes, one lattice, both arms.
