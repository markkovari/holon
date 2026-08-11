# 0063 — A TTL is cheaper than coherence

Status: accepted, built, and **off by default**. Answers
[ADR-0059](0059-the-read-mirror-lost.md) with the workload
[ADR-0062](0062-what-a-real-application-asks-the-store-for.md) measured.

## The idea ADR-0059 got right, and the part it did not need

The read mirror kept each node's copy fresh with a NATS `watch()`, and lost by
2.3×. The reason was structural: the watch delivers every write to every node, so
upkeep scales with the **fleet's write rate**, and the component it was measured on
wrote once per request.

ADR-0062 measured a real application: **264 reads per write**, and the hottest key
in the run took 399 697 reads against 1 write. At that ratio the expensive half of
the mirror — learning *immediately* that someone else wrote — is insuring against
something that hardly ever happens, and billing every write in the fleet for it.

So drop that half. An entry lives for `--kv-cache-ms` and then it is gone. No
watch, no invalidation message, no subscription, nothing whose cost scales with
anyone else's writes. The staleness bound is the TTL and nothing else.

## Measured

Conduit, `oha -c 20`, NATS JetStream, clean store for each arm.

| route | no cache | `--kv-cache-ms 1000` | | memory backend, no cache |
|---|--:|--:|--:|--:|
| `GET /api/articles` | 6 167 | **17 955** | 2.9× | 17 522 |
| `GET /api/articles/{slug}` | 10 392 | **19 107** | 1.8× | 19 270 |
| `GET /api/articles/feed` | 3 940 | **16 409** | 4.2× | 15 337 |
| `GET /api/user` | 14 259 | **19 206** | 1.3× | 19 649 |
| `GET /api/tags` | 14 342 | **19 456** | 1.4× | 19 957 |
| `POST /api/users/login` | 214 | 216 | 1.0× | 212 |
| create | 364 | **1 113** | 3.1× | 784 |
| `GET /` (no KV) | 21 709 | 20 250 | 0.93× | 20 967 |

```
15 211 293 hits, 46 095 misses (99.7% served), 5 364 entries held
```

99.7% against the 99.8% upper bound ADR-0062's model predicted, which is the
model earning some credit.

**The last column is the point.** Cached-on-NATS lands on the in-memory backend's
numbers, route for route. The read cost of durable storage is, for this workload,
gone.

Two rows that do not move, and both are informative:

- **login, 214 → 216.** It is argon2, not storage — the same 1.0× it shows between
  the NATS and memory backends. The slowest route in the app is untouched by any
  of this, and no caching work will ever touch it.
- **`GET /` (no KV), 21 709 → 20 250.** Nothing to cache and it went slightly
  *down*. Run-to-run noise on this route spans that gap, so this is not evidence
  of overhead — but it is not evidence of none either, and a route that does no
  storage work should not be quoted as either.

## What it costs, stated where nobody can miss it

**A write on another node is invisible until the entry expires.** Within a node,
writes invalidate their own key before the write is reported done, so
read-your-own-writes holds locally. Across nodes it does not.

That interacts with [ADR-0027](0027-a-spread-app-needs-a-shared-store.md), which
refuses to spread a stateful app over node-local stores precisely to prevent
silent divergence. This flag reintroduces divergence — bounded by the TTL, but
real — on a store the platform still reports as shared. **Being off by default is
the whole mitigation.** An operator turning it on for a spread stateful app is
accepting exactly what ADR-0027 refuses to accept by accident.

`increment` is never served from cache and always invalidates: a cached
read-modify-write is a lost update, not a stale read. `list_keys` is never cached
(its answer changes with any write to any key in the bucket, which needs
bucket-level invalidation this design does not have) and `exists` is not either
(metadata, not a value).

## What the conformance run does and does not prove

The RealWorld suite — 154 sequenced requests that create things and immediately
read them back — passes 13/13 at both 100 ms and 1000 ms.

That proves the **local** invalidation is right. It cannot prove anything about
coherence, because it is one node, and on one node this design is defined to
preserve read-your-own-writes. **The cross-node staleness cost remains unmeasured.**
The honest next measurement is two nodes serving one app with a writer on each.

## Why not fix the N+1 instead

`feed` at 3 940 rps against `tags` at 14 342 is per-article author and favorite
enrichment — an application-level N+1 that ADR-0062 also found, and removing a
round trip beats caching one. It remains the better fix and it is not this one:
it needs changes in `conduit-domain`, it helps only the app that gets changed, and
it does nothing for the 23% of gets that are auth introspection. This is one flag
on the host and it helps every app.

Both, ideally, in that order of preference and this order of effort.
