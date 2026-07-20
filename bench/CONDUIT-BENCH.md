# Conduit bench (round 13) — RealWorld app path on the native host, NATS vs memory KV

The whole *app* request path for the RealWorld showcase (CONDUIT.md). Every
request below goes browser → hyper → wasmtime → `conduit_domain.composed.wasm`
(conduit-domain + auth-guard + record-store + slug) → `wasi:keyvalue` backend.
Same harness shape as the helpdesk round, so the numbers are comparable.

- Host: `just host-conduit` (vet-host binary, release), Apple M4 (10 cores), macOS
- Load: `oha -c 20`, 10s per route (3s for create), localhost — `bench/conduit-bench.sh`
- Backends: NATS = JetStream KV in Docker on the same machine (durable,
  disk-persisted per write); memory = in-process HashMap
- Seed: 1 author + 3 articles, 1 reader who follows the author and favorites an
  article; tokens pre-minted (login is the only unauthenticated write measured)

| route | work per request | NATS rps | NATS p50/p99 ms | memory rps | mem p50/p99 ms |
|---|---|--:|--:|--:|--:|
| `GET /api/articles` | scan + per-article author + favorites enrich | 88 | 231.7 / 341 | 2 675 | 7.4 / 11.5 |
| `GET /api/articles/{slug}` | find-by slug + author + favorites | 184 | 109.6 / 161 | 2 671 | 7.5 / 11.2 |
| `GET /api/articles/feed` | follows find-by + scan + enrich | 50 | 401.4 / 631 | 2 813 | 7.0 / 11.1 |
| `GET /api/user` | introspect + find-by subject | 330 | 61.1 / 88 | 2 787 | 7.2 / 10.7 |
| `GET /api/tags` | scan tagList (no enrich) | 328 | 61.1 / 92 | 2 781 | 7.2 / 10.7 |
| `POST /api/users/login` | argon2 verify + session mint | 99 | 206.1 / 328 | 188 | 105.9 / 200.5 |
| `POST /api/articles` | introspect + count + slug find-by + create(2 idx) | 47 | 422.8 / 799 | 916 | 18.8 / 53.2 |
| `GET /` (usage) | wasm routing, no KV | 2 844 | 7.1 / 10.6 | 2 739 | 7.3 / 11.0 |

## Takeaways

- **The component is not the cost** (same finding as helpdesk). With the memory
  backend every read sits at ~2.7k rps / ~7 ms p50 at c=20 — list, single,
  feed, current-user and tags are indistinguishable, so wasm + composition +
  auth introspection overhead is flat and small. `GET /` (no KV at all) lands in
  the *same* band, which is the tell: the wasm path is not the bottleneck.
- **On NATS, cost = number of KV round-trips.** Each `wasi:keyvalue` op is a
  synchronous JetStream round-trip. The read ladder is entirely explained by op
  count: `tags`/`user` (~1 lookup) → 330 rps; single article (slug + author +
  favorites, ~3) → 184; **list/feed** do per-article author+favorite enrichment
  (**N+1**) → 88 / 50. The memory column proves the enrichment logic itself is
  free; NATS just bills each hop.
- **Login is argon2-bound either way** (~100–200 ms of hashing at c=20), NATS
  only adds the session write. Same shape as the auth-guard rounds and helpdesk.
- **Create is write-bound**: introspect + `count` + slug `find_by` + record
  create with two secondary indexes (each a read-modify-write, no CAS in the
  `wasi:keyvalue` contract). 47 rps on NATS / 916 on memory.
  - *Caveat:* this bench POSTs an identical title repeatedly, so every create
    after the first hits the `unique_slug` collision path — an O(n) scan over
    all articles (a deliberate `ponytail:` shortcut). Distinct titles skip it
    (one `find_by`). The 3 s window bounds the growing scan; treat create as a
    worst-case floor, not steady state.
- **Path to faster NATS reads** (not taken — the showcase favours simple code):
  the list/feed N+1 is the obvious target — batch the author/favorite lookups,
  denormalise author name + a favorites counter onto the article, or cache
  introspection host-side. All are contract-compatible; none are needed for the
  conformance suite, which passes regardless of backend.

## Repro

```bash
docker compose -f infra/compose.yaml up -d nats     # for the NATS column
just compose-conduit                                # build + compose the app
cd host && cargo build --release --bin vet-host && cd ..
bench/conduit-bench.sh memory                       # in-process HashMap
bench/conduit-bench.sh nats                         # JetStream KV
```
