# ADR-0093 — a lesson is reached through its interface

*[ADR-0090](0090-a-lesson-is-about-a-capability-not-a-sentence.md) was right about
the key and wrong about needing a second store.
[ADR-0091](0091-one-store-one-schema.md) was right about the store and deferred
the key. This is the half that was left.*

**Status: accepted**, and built. Supersedes neither — it completes both.

## What was actually deferred

ADR-0091 merged the stores and, in its own words, left `lesson -about-> interface`
as *"a real edge in the schema draft"* and a note. Retrieval stayed as it was:

```surql
SELECT ... FROM memory WHERE tags CONTAINSANY $ifaces
```

A full table scan of the one half that ADR-0091 measured as **not scaling** — 55 ms
at 200,000 lessons against roughly 12 ms for edge traversal, with no index that
fixes it. And `knowledge:memory`'s own `recall` takes a **goal string** and matches
on it; the interface tags are a secondary filter, documented as *"also return
lessons tagged with any of these, whatever the goal says."*

That ordering is backwards, and ADR-0090 already paid for it: *"both paid runs so
far died on facts about an INTERFACE that were indexed by the wording of a goal."*

## The decision

**The interface is the retrieval key. The goal's wording is the fallback.**

`lesson -about-> interface` is a real edge, written by the projection, and
`just lessons-for` traverses it instead of scanning. An app reaches what previous
runs learned by walking its own composition:

```
app -carries-> artifact -imports-> interface <-about<- memory
```

Nothing in that path mentions a goal, a topic, or a word anybody chose.

## Why the edge is DERIVED, and why that is not a contradiction

The edge is written by `comp-capgraph`'s projection and aged out by generation like
every other derived row, which looks like the projection reaching into the
accumulated half that ADR-0091 exists to protect.

It is not. **A tag on a lesson is the accumulated fact; the edge is an index over
it.** `about` is its own table: ageing out a generation of edges leaves every
lesson exactly as it was, and the edge is recomputed from `memory.tags` on the next
build. The generation delete names `about` and has never named `memory` — that list
is the isolation, and it is unchanged.

The projection does now emit `DEFINE TABLE IF NOT EXISTS memory`. A definition is
not a row: it exists because `SELECT ... FROM memory` is an error on a database
where nothing has been learned yet, which is every fresh install.

## What the shape cost

Three formulations were tried against SurrealDB v3.1.3 and two of them are
landmines a happy-path test would have shipped:

| shape | fails on |
| --- | --- |
| iterate `interface`, relate a **list** of lessons to each | always — `RELATE` takes a list as its target, never as its source |
| iterate `memory` directly | an empty pool (`NONE`), and any lesson whose tags name no interface (empty target) |
| **build `{lesson, interfaces}` rows, skip the empty, relate the rest** | nothing found |

The middle one is the instructive failure. It passes every test that seeds a lesson
which matches something, and breaks on the two states that are **normal**: a fresh
install, and a lesson about an interface that has been retired — which under
append-only interfaces is not an error at all, since a retired interface leaves its
lessons behind. `capgraph_store.rs` now asserts both, plus a real lesson afterwards,
so the two cannot pass by the projection having quietly stopped writing.

The working shape iterates the pool rather than the 80 interfaces. That is a
**rebuild** cost, paid by `just capgraph-store` and never on a read, which is the
entire reason this is a projection.

## Measured

Against SurrealDB v3.1.3, the full projection of this repository:

| | |
| --- | --- |
| statements | 1150, **0 rejected** |
| whole projection | **55 ms** |
| nodes and edges | 94 interfaces, 152 artifacts, 48 apps, 306 imports, 164 exports, 348 carries |
| `just lessons-for vet` | returns the lesson about `csv:codec/codec`, and not the one tagged with an interface nothing imports |

The falsifying test is in `capgraph_store.rs` and is narrow on purpose: **the
traversal returns exactly what the scan returned.** Fewer means the index is lossy
and the scan has to stay; more means it is matching things the tags do not say.

## What this does not do

`recall` is unchanged — it still takes a goal string, and a branch still retrieves
by wording. Changing that is a WIT change to `knowledge:memory` and belongs to its
own slice; this one makes the edge exist, correct and measured, so that slice has
something to stand on.

Nothing is deleted. The scan still works and `tags` is still the fact of record —
which is what makes this reversible: drop the `about` table and the previous
retrieval path is exactly as it was.

And embeddings keep their place. A lesson about a technique that touches no
interface is real, and the fallback is what it is for.
