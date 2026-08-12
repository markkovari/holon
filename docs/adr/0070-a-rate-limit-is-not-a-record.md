# 0070 — A rate limit is not a record

Status: accepted, and built. Acts on what `bench/FLEET-BENCH.md` found, and
corrects the fix that benchmark proposed.

## What the fleet bench pointed at

Spreading `gate-domain` over three machines put `/api/ratelimit` at 86 rps with
distinct keys. The benchmark blamed round-trip count and proposed using
`wasi:keyvalue/atomics::increment`, "one round trip, no retry loop".

**That proposal was wrong.** A token bucket carries `(tokens, updated_ms)` and
refills against the clock; `increment` adds an integer to an integer. It cannot
express the state, let alone the refill. The measurement was right about the
cost and wrong about the cure — written in the same hour as the run, which is how
that happens.

## What the cost actually was

The bucket was stored as a `record`. So each request:

- **found** it — a secondary-index lookup: index manifest, index chunk, then a
  batched record fetch;
- **wrote** it — a guarded record update, plus index maintenance, which drops the
  old index entries and adds the new ones, each a chunked-list read-modify-write.

Counted with `--kv-profile` rather than estimated:

```
old   987 563 reads + 169 405 writes over 13 588 requests   ≈ 85 store operations per request
new                                                                                        2
```

Eighty-five. For a value with no identity beyond its key, that is never listed,
never queried by field, and is overwritten on every single request. Every one of
those extra operations exists to maintain indexes nothing reads.

## A bucket is keyed state, so it is stored as keyed state

`comp:store/cas` directly on `rl_{key}`: the guarded read gives the revision, the
guarded write lands only if nothing moved. Same optimistic-concurrency loop as
before — it just stopped paying for a record's ceremony.

```
                        old      new
distinct keys          1 480    7 043 rps    4.8×      p50 9.7 → 2.7 ms
one hot key              785    2 835 rps    3.6×      p50 16.9 → 4.5 ms
```

This is [ADR-0062](0062-what-a-real-application-asks-the-store-for.md)'s own
conclusion, applied: *removing a round trip beats caching one*. The old path's
profile says a perfect cache would have served 94.8% of its reads; the new path
does not have those reads at all, which is strictly better than serving them
quickly.

## The regression it caused, and the arithmetic that explains it

The first version failed **1 800 of 18 575** hot-key requests with `503
contended, retry`. Not a fluke:

> With N writers on one key, exactly one wins per round, so a request that
> retries K times fails with probability `((N-1)/N)^K`. At N=20, K=40 that is
> **12.9%** — against a measured 9.7%.

The budget had always been 40 rounds. It never mattered because
`record-store::update` ran its *own* 40-try loop inside each one, so the
effective budget was 1 600. Doing two operations per attempt instead of
eighty-five silently cut it by 40×.

`CAS_TRIES` is now 200 — `0.95^200` is 0.0035%, and measured 1 failure in 17 008.
Each attempt is cheap enough that the whole 200-round worst case is less work
than one old attempt.

The general lesson is worth more than the fix: **making an operation cheaper
changed a retry budget nobody had written down.** The nesting that made 40 mean
1 600 was invisible in both components.

## What did not change

`throttle` (GCRA) and `batch` still store records, and should: a batch has
identity, is fetched by id, and is listed. Only the bucket was keyed state
wearing a record's clothes.
