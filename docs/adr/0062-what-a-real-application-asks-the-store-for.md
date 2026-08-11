# 0062 — What a real application asks the store for

Status: accepted, as a measurement. It does not build a cache; it supplies the
number [ADR-0059](0059-the-read-mirror-lost.md) decided without.

## The gap this closes

Every caching decision this platform has taken was measured on `gate-domain`: a
rate limiter that does a read-modify-write on **every request**. ADR-0059 rejected
the read mirror on it and gave a structural reason — the mirror's upkeep scales
with the fleet's *write* rate, so a write-per-request workload pays the upkeep and
collects none of the benefit.

The reason is sound. What was never checked is whether any real application looks
like that. `CURRENT.md` has said so plainly for two rounds: *"No real application
has ever been profiled, only a benchmark component chosen because it hammers
storage — which is the least representative shape for every caching decision
above."*

## The instrument

`comp-host --kv-profile` wraps whichever backend `--kv` chose and counts every
operation: calls, mean latency, and how many reads a cache would have served.

The hit-rate model is deliberately an **upper bound**. A key is warm once read;
any write to it makes it cold. A read of a warm key is one a cache with infinite
capacity and perfect invalidation would have served. `increment` never counts as a
hit — a cached read-modify-write is a lost update, not a stale one, which is the
same exclusion ADR-0059's mirror made. Two buckets sharing a key name are two
entries, so the model cannot report a cross-tenant hit.

It is off by default and not on the request path when off: the backend is handed
out unwrapped.

## The application

Conduit — the RealWorld spec, the one showcase validated against an external suite
(154 requests, 13/13 files). Two workloads, because they are different questions:
the conformance suite is a functional sequence full of setup, and
`bench/conduit-bench.sh` is steady-state load over the read routes.

### Conformance suite, 154 requests

```
get 1326   set 570   delete 135   increment 1
reads 1326, writes 706 — 65.3% read
a perfect cache would have served 772/1326 gets (58.2%), holding 111 keys
```

### Under load, `oha -c 20`, NATS JetStream

```
op          calls        mean us     share
get       5131194        169.1       99.6%
set         15410        401.7        0.3%
delete       4026       1449.7        0.1%

reads 5131194, writes 19436 — 99.6% read
a perfect cache would have served 5121547/5131194 gets (99.8%), holding 1926 keys
```

**264 reads per write.** `gate-domain` is roughly one write per request.

## What follows

| | gate-domain (ADR-0059) | conduit under load |
|---|---|---|
| writes | one per request | 0.38% of ops |
| what a mirror's upkeep tracks | the fleet's write rate | 264× smaller |
| reads a perfect cache serves | — | 99.8% |

The arithmetic on the NATS run: 5.13 M gets at 169 µs is **868 seconds** of
JetStream round trips inside a 73-second bench at concurrency 20 — about **59% of
the whole request-time budget**, and a perfect cache removes 866 of those 868
seconds.

So ADR-0059's rejection was **workload-specific, not structural**, and its own
stated reason says which workloads it does not cover. On this shape a read cache
is the single largest available win on the request path.

Three things this does NOT establish, and none of them are small:

1. **Nothing here models a fleet.** One process, one node. A multi-node cache has
   to notice another node's writes, and that coherence traffic is precisely what
   killed the mirror. A 99.8% single-node hit rate says the *demand* is there, not
   that a distributed design will capture it.
2. **On the `memory` backend a hit saves 1.5 µs**, which is nothing. This is a
   NATS-shaped win only — it buys back a round trip, not a lookup.
3. **A working set of 1926 keys is the bound for one app's bench**, not a capacity
   plan. A real tenant mix has to be sized, and an evicting cache does not get the
   upper-bound hit rate.

## Also measured, unintentionally

`bench/CONDUIT-BENCH.md` (round 13) records the memory backend at 2 675 rps for
`GET /api/articles`. The same route on the same machine now serves **17 522 rps**,
and 6 040 on NATS where round 13 had 88. Pooling by default (ADR-0054) and the
ADR-0057 work land somewhere in there. That table is stale by roughly 6×, which is
worth knowing before anyone quotes it.

## Repro

```bash
just compose-conduit
cd host && cargo build --release --bin comp-host && cd ..
PROFILE=1 bash bench/conduit-bench.sh memory
NATS_URL=nats://127.0.0.1:4299 PROFILE=1 bash bench/conduit-bench.sh nats
```
