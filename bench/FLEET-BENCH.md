# Fleet bench — three real machines, replicated storage

The first run against actual hardware since the `comp-bench` refactor, and the
first ever with `--kv-replicas 3`. Closes the caveat ADR-0067 wrote down: *"three
processes on one machine prove the code, not the hardware."*

- **picur** (this Mac, M4, 10 cores) · **bobocat** (Mac, 8) · **csatapaci** (Mac, 12)
- Joined over **Tailscale**, direct (not relayed). malna (Pi, Linux aarch64) was
  up but runs a different architecture, so it sat this round out.
- One JetStream cluster across all three on a dedicated port, `--kv-replicas 3`.
  Each machine also runs the user's own unrelated NATS; this used 4232/6232 to
  keep out of its way.
- `gate-domain` on each machine, single-app lane, same tenant/app — so all three
  share one bucket.

## 1. The durability work cost nothing (local, same machine as ADR-0063)

| route | ADR-0063 | after CAS + revisions | |
|---|--:|--:|--|
| `GET /api/articles`, no cache | 6 117 | 6 052 | −1.1% |
| `GET /api/articles/feed`, no cache | 3 897 | 3 860 | −0.9% |
| `GET /api/articles`, cached | 17 855 | 17 776 | −0.4% |
| `GET /api/tags`, cached | 19 423 | 19 080 | −1.8% |
| create, no cache | 352 | 394 | +12% |

Revisions on every write and two CAS calls inside `record-store::update` are
inside run-to-run noise. `create` swings ±12% between any two runs.

## 2. One budget, three machines, and one of them killed

```
one shared counter, spent from each machine in turn
  picur 999  bobocat 998  csatapaci 997
  picur 996  bobocat 995  csatapaci 994
  picur 993  bobocat 992  csatapaci 991

  *** csatapaci's NATS server killed — a whole machine's copy of the store ***

  picur 987  bobocat 986  csatapaci 985
  picur 984  bobocat 983  csatapaci 982
  picur 981  bobocat 980  csatapaci 979
```

Zero errors, no reset, no gap. Two things this proves that the single-machine
cluster in ADR-0067 could not:

- **The data survived losing a machine**, not just a process.
- **csatapaci's HOST kept serving** after its local NATS died, because it was
  started with all three URLs and failed over to another machine's server. That
  is the multi-URL change from ADR-0067 doing its job.

## 3. What it costs to spread over a tailnet

| | rps | p50 | p99 |
|---|--:|--:|--:|
| `GET /` (no storage), picur **loopback** | 41 707 | 0.5 ms | 0.9 ms |
| `GET /` (no storage), over the tailnet | 1 230 | 12.9 ms | 106.6 ms |
| rate limit, **distinct** keys, R3 across machines | 86 | 218 ms | 569 ms |
| rate limit, **one hot** key, R3 across machines | 22 | 851 ms | 3 506 ms |

Read this as two multipliers on top of a fast request path, not as a platform
number:

1. **The tailnet costs ~34×** on a request that touches no storage at all
   (41 707 → 1 230). WireGuard on macOS, 20 connections. Nothing to do with comp.
2. **Round-trip count costs the rest.** One `/api/ratelimit` is ~6 KV operations
   — the index manifest, a chunk, the guarded read, the guarded write, index
   maintenance — and every one is now a quorum write across three machines. This
   is ADR-0062's finding again: *cost = number of round trips*, now with each
   round trip 200× more expensive than loopback.

The same component on one machine with local storage serves ~6 000 rps. The gap
is geography and chattiness, in that order.

## 4. The hot key is the app using the wrong primitive

22 rps on one key is not the platform being slow, it is twenty writers doing
optimistic concurrency on one value across a quorum — on top of a request that
was making **85 store operations**, because the bucket was stored as a `record`
with indexes nothing reads.

> The original text here proposed `wasi:keyvalue/atomics::increment` as the fix.
> That was wrong: a token bucket carries `(tokens, updated_ms)` and refills
> against the clock, which an integer increment cannot express. The right fix was
> `comp:store/cas` on one key — 85 operations down to 2, and 4.8× throughput.
> See [ADR-0070](../docs/adr/0070-a-rate-limit-is-not-a-record.md).

## 5. A correction: the CAS backoff is not the clear win claimed

ADR-0069 adopted wasmCloud's exponential backoff and called it "the difference
between a retry loop and a retry storm". Measured on one hot key, c=20, R3
across machines:

```
backoff OFF (old)    14 rps   p50 1 095 ms   p99 7 490 ms
backoff ON  (5 ms)   12 rps   p50 1 737 ms   p99 4 007 ms
```

**Throughput is unchanged within noise** (73–85 samples per arm — small, and the
run-to-run spread on this test is wide). What backoff actually buys is the tail:
p99 roughly halves. What it costs is the median, which roughly doubles.

That is a defensible trade and not the one advertised. The claim in ADR-0069 was
written from reading wasmCloud's source, not from measuring this fleet.

## Repro

The cluster is stood up by hand — three `nats-server`s with a shared
`cluster { name }` on 4232/6232, one `comp-host --kv nats --kv-replicas 3
--nats-url <all three>` per machine. Everything was removed afterwards; nothing
is left installed on bobocat or csatapaci.
