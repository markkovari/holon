# ADR-0091 — one store, one schema

*ADR-0090 argued the two graphs must stay separate and meet at query time: "the
join is the missing edge, not a missing store." That was written before anyone
tried to hold the edge. Holding it means every question worth asking crosses a
component boundary, and the boundary buys nothing that three cheaper mechanisms
do not buy better.*

**Status: accepted, and the first slice is built. Supersedes ADR-0085. Reverses
ADR-0090's central claim, and keeps everything else it said.**

> Verified against SurrealDB v3.1.3: `just capgraph-store` writes 94 interfaces,
> 150 artifacts, 47 apps, 303 import edges, 162 export edges and 344 carries
> edges; `just lessons-for vet` returns a lesson about `csv:codec/codec` — 0090's
> own example — with nothing in the query mentioning CSV or anything veterinary.
> The isolation was tested rather than asserted: lessons seeded before three
> successive rebuilds were still there afterwards, and only one generation of
> derived rows survives each run.

## What is being reversed, exactly

ADR-0090 is right that the capability graph is *derived* and the knowledge pool
is *accumulated*, right that those have different truth models, and right that a
lesson is about an interface rather than about the wording of a goal. None of
that changes here.

What changes is the conclusion drawn from it. 0090 concluded that two truth
models require two stores. They require two *lifecycles*, which is not the same
thing. A store boundary is one way to enforce a lifecycle difference; it is the
most expensive way, and it is the one that makes the join — the thing both 0085
and 0090 identify as the actual missing piece — permanently awkward.

ADR-0085 asked "why a second pool and not a bigger one" and answered: isolation.
It also said, in its own status line, that the isolation question "is not mine to
answer alone." It has now been answered (see *visibility*, below), and the answer
does not need a second pool.

## The decision

`knowledge-memory` absorbs `knowledge-graph`. One component, one SurrealDB
connection, one schema, holding what were four graphs:

- **components and artifacts** — derived, from `comp-capgraph`
- **goals and runs** — accumulated, graph-first with the file rendered from it
- **knowledge** — accumulated, as blessed events

Two components on one database with two schemas is the split store with extra
steps. `knowledge-memory` absorbs rather than the reverse because it is the one
that already owns the write rule (ADR-0084) and the retrieval.

**One store means one set of coordinates: `comp` / `goalmemory`.** That is
`knowledge-graph`'s default namespace and the database `comp-goalrun` rewrites the
memory app to, so lessons, runs, attempts and capabilities all land there — and
the projection has to land there too. It did not: `just capgraph-store` defaulted
to `holon`/`holon` for months, which made the join below correct and unrunnable at
the same time. The store was merged in the schema and split in the deployment,
which is the harder half to notice, because every test wrote both halves into a
database it had started itself. `capgraph_store.rs` now asserts the *coordinates*
against the two files that declare them, without a database, since a mismatch
between files is not something booting Docker can see.

The narrowness matters: `contracts` and `memorysparse` stay separate databases on
purpose, for the reasons `fixtures/knowledge-memory-sparse.yaml` gives. "One
store" is a claim about the graph and the pool, not a plan to collapse every
database in the repository.

## The lifecycle difference, without a store boundary

Three mechanisms replace what the boundary was doing.

**Generation stamping.** Derived nodes carry a generation number. A rebuild
writes a new generation; old derived nodes age out. Nothing is deleted to make a
rebuild safe, so a rebuild can never reach the accumulated half — the failure
mode a shared store would otherwise have is one bad `DELETE` from eating the only
data in the system that cannot be recomputed.

**The projection rule.** `comp-capgraph` remains the source. What lands in
SurrealDB is its projection: written only by a rebuild, never hand-edited, always
safe to drop. This is what makes generation stamping meaningful — a projection
with hand-preserved fields is a projection nobody can trust, and it would forfeit
the whole arrangement.

The rule has a consequence worth stating plainly, because it is the one that
constrains everything downstream: **no status may be stored on a derived node.**
An artifact's outcome — gates passed, times reused — is history, and history on a
droppable node is history you will drop. Outcome is therefore *counted from
events*, never asserted as a field. There is no status to lose and none to keep
in sync.

**Split visibility.** Structure is visible to every branch the moment it lands.
Lessons are snapshot-isolated per branch. This is 0085's isolation question,
answered: structure is *true* regardless of who learned it, and hiding it makes
twenty branches rediscover the same facts twenty times badly — the exact waste
0085 was written to stop. Lessons are *beliefs*, and a swarm whose branches all
read each other's early wrong beliefs converges fast on being wrong together,
which forfeits the diversity that justifies running twenty of them.

Different truth models, different visibility. That is the derived/accumulated
line still earning its keep after the stores merge.

## Considered and rejected

**Four stores with one federating query surface.** 0090's design, generalised.
Rejected because the federation layer is the join, and building a layer whose
only job is to hide a boundary is cheaper than removing the boundary only if the
boundary buys something. It buys lifecycle isolation, which generation stamping
buys for a column.

**Two stores, split derived from accumulated.** The honest version of 0090, and
the one most defensible on paper. Rejected for the same reason: the primary
retrieval path (below) crosses the line on every single query.

**Preserving selected fields across a rebuild.** The obvious fix once outcome
needs to survive a projection rebuild. Rejected outright — it is what turns a
droppable projection into a store nobody dares drop.

## What the merge buys

Retrieval goes graph-first. An app's imported interfaces are a structural fact;
the lessons attached to those interfaces are the candidate set; vector and
lexical ranking then operates *within* a set that is already relevant, rather
than being asked to guess relevance from a goal's wording. ADR-0090 opens by
naming that guess as the thing that killed two paid runs.

Text-only retrieval remains as the cold-start fallback, for a goal touching
nothing the graph knows yet. What is explicitly not done is running both and
merging the results — the text half would re-admit everything the graph half
filtered out.

## The first slice

Persist `comp-capgraph` into the merged store and make one cross-graph query
work: *the lessons about the interfaces this app imports*. Proven by a `just`
target that prints relevant lessons for an app nobody named in the query.

That is the smallest thing that cannot be done today and cannot be faked by the
joined design this ADR reverses. If it is awkward to build, the reversal was
wrong, and that is worth finding out before goals, events and outcome counting
are built on top of it.

## What it costs, measured

`reconciler/tests/capgraph_stress.rs`, against the pinned `surrealdb:v3.1.3`:

| | |
| --- | --- |
| graph 30x larger (291 → 7,568 nodes, 809 → 25,813 edges) | join **1.0x** slower |
| pool 40x larger (5,000 → 200,000 lessons) | join **5x** slower |

The graph half is effectively free — the traversal is indexed and the hops cost
nothing at any size this repository will reach. **All of the cost is the pool**,
and it is a full table scan: `tags CONTAINSANY [...]` does not use an index. That
is not a missing `DEFINE INDEX` — one was defined and measured and changed
nothing, and `EXPLAIN` reports `TableScan` either way.

So retrieval currently costs O(everything the swarm has ever learned), whether or
not it is relevant. At 200,000 lessons that is 55ms and nobody cares. It is worth
writing down because the fix is already sketched above and was not taken: the
schema draft has `lesson -about-> interface` as a real edge, and the
implementation matches on `knowledge-memory`'s existing `tags` strings instead,
because duplicating them as edges bought nothing at the size the pool is now.
Measured side by side on the same 200,000 lessons, same 400 results: **12ms by
edge, 55ms by scan**, and the gap widens with every run that finishes.

Not changed now. The trigger to change it is the pool passing roughly a million
lessons, or retrieval showing up in a run's latency — and the stress test that
produced these numbers is what will say so.

## Deliberately not decided here

The event vocabulary and outcome counting are append-only and cheap to change;
writing them down now would record a guess as a decision. They get their own ADR
once the slice above runs.
