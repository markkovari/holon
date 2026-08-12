# 0077 — Asking the same question twenty times

Status: accepted, and built. Removes the N+1
[ADR-0062](0062-what-a-real-application-asks-the-store-for.md) found and
`FLEET-BENCH` kept pointing at.

## The finding, twice

ADR-0062 measured Conduit's `feed` at 3 940 rps against `tags`'s 14 342 and named
the cause: per-article author and favorite enrichment. Its own conclusion was
*"removing a round trip beats caching one"*, and then the round trip stayed.

Reading `article_json`, each article on a page asked for three things:

* its favorites — one lookup per article, and genuinely per article;
* its author — one lookup per article, but a page is usually **one author**;
* whether the viewer follows that author — and this one is the joke:
  `find_by(FOLLOWS, "follower", viewer)` is **byte-identical for every row on the
  page**. A twenty-article page ran the same query twenty times for one answer.

`comment_json` had the same shape, and comments are a list too.

## The fix is not a cache

A per-request memo. The distinction matters: a component instance is per-request
(ADR-0037), so this cannot outlive the response it was built for and cannot go
stale. There is no invalidation, no TTL, and none of ADR-0064's coherence
questions — it is not caching an answer, it is not asking the same question twice
inside one request.

* the author lookup collapses to one per **distinct** author on the page;
* the follow lookup collapses to **exactly one**, whatever the page size.

Favorites are untouched: each article genuinely has different favorites, and
batching those needs a `find-by` over many values, which the `record:store`
contract does not have.

## Measured in operations, not rps

The machine was visibly noisier during this work — `tags` moved 13 852 → 9 763
between two runs of the same binary — so throughput was the wrong instrument.
`--kv-profile` counts store reads, which the machine's mood cannot change. Same
seed, same 50 `GET /api/articles/feed`:

```
before   1 763 store reads
after    1 151 store reads

12 fewer reads per feed request — 35% fewer over the run
```

That gap widens with page size: the follow lookup is N→1, so a twenty-article
page saves nineteen of them rather than two.

RealWorld conformance still passes 13/13, which is the assertion that matters for
a change that touches every article and comment response.

## What is still N+1

Favorites, deliberately. Fixing them means either a contract change (`find-by`
over a list of values) or denormalising a count onto the article — and a
denormalised counter is a second source of truth to keep in step, which is the
class of bug this session has spent its time removing rather than adding.
