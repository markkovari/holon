# ADR-0080 — the graph remembers

*A knowledge graph over SurrealDB's HTTP API, as a component.*

## The gap

An environment can now fork itself (ADR-0078, 0079). What it cannot do is
remember anything about the fork: which file it changed, which symbol that file
defines, which attempt it descended from, what it concluded and why.

`records:store` is the obvious place to put that and the wrong one. It answers
"find the rows where `field = value`" — one secondary index per question, written
in advance. A graph loop asks the other kind of question: *what is two hops from
here*. `favourites → articles → authors` was N+1 in `conduit-domain` for exactly
this reason (ADR-0077), and that was two hops over data whose shape was known.
Traversal over data whose shape an agent is still discovering is not something an
index over flat records will do.

So: a real graph store.

## A component, not a host capability

It needs a network socket and nothing else, and the host already grants one.
That makes it a component:

- deployable mid-run, like anything else in the catalogue;
- swappable for a different backend without touching the host;
- bounded by the deployment's egress allow-list rather than by trust (ADR-0008).

The alternative — a native SurrealDB driver in `comp-host` — is what alpha-swarm2
does, and it is why its `knowledge-base` is native-only. A host rebuilt to add a
database driver is a host whose isolation has to be re-verified, and ADR-0079
already recorded that we prefer to fork apps rather than hosts.

The URL and namespace are `wasi:config`. The password is a SECRET, read through
`comp:secrets/reader` — a manifest must never carry one (ADR-0010), and a config
map is the most-dumped namespace there is (ADR-0051).

## What the contract is

Four verbs and an escape hatch:

    upsert(node)                                 create or replace, idempotent
    get(kind, id) -> option<node>                absence is an answer
    relate(from, edge, to, properties)           properties travel on the EDGE
    neighbours(kind, id, edge, dir, limit)       the hop records cannot do
    query(surql) -> string                       for what the four cannot ask

`relate` upserts both ends first. A graph that refuses an edge because a node is
not there yet forces every caller to order its writes, and an agent exploring a
graph does not know the order in advance.

Injection is the risk in a component that builds a query language by string, so
nothing a caller supplies is interpolated raw: an id is quoted with SurrealDB's
own bracket form, a kind or an edge has to *be* a table name (`[a-zA-Z0-9_]`, no
quoting, no creativity), and properties are re-serialised from parsed JSON so a
value cannot carry syntax. `query` is the exception, says so in the WIT, and is
there because an interface that has to be extended for every new question is an
interface that gets bypassed.

## Four things the documentation did not say

The first draft was written against the docs and was wrong in four places. Each
was found by running statements against a live SurrealDB 3.1.3 and reading the
answers, and each is now pinned by a test carrying the captured shape:

1. **A fresh server has no namespace and will not make one.** The first write
   comes back `The namespace 'comp' does not exist` and stops. A component that
   requires an operator to pre-provision works on the maintainer's machine and
   fails on a fresh database, so a missing namespace is defined and the statement
   retried — once, because a second failure is the real answer.

2. **Ids go out in angle brackets and come back in backticks.** SurrealDB
   re-quotes on the way out with its own preferred form, and only when the id
   needs quoting at all. Stripping only what we sent meant no path-shaped id ever
   round-tripped.

3. **A read of a table nobody has written is an ERROR, not an empty set.**
   `SELECT * FROM nosuch:x` answers `The table 'nosuch' does not exist`. For a
   graph an agent is still building, the first question about a kind always
   precedes the first write of it — so an empty graph would have looked like a
   broken one. Reads map it to empty; writes still do not.

4. **`<->` is not a traversal.** "Either direction" is the two queries unioned.

## And one bug in the host

The end-to-end test needed a component on one node to reach a database on
another port of the same box. It could not: the host denies its own address to
stop a component calling back in as though it were a client, and it was denying
the whole **IP** rather than its **socket**.

On a lattice node that distinction is invisible — the address is private and
denied by range anyway. It only appears under `--allow-private-egress`, where it
took out every colocated service a component was meant to reach while protecting
one port. `denied_addrs` is now `Vec<SocketAddr>`, and the deny is the listener,
not the machine.

## How it is known to work

Unit tests cover the SurrealQL built and the JSON read back, against shapes
captured live. They cannot cover what is between the two, and that is the half
this repo has got wrong before — `comp:secrets/reader` shipped unlinked and every
claim in its ADR was untested until something ran it (ADR-0061).

So `reconciler/tests/graph.rs` starts a real SurrealDB — a **pinned container**,
`surrealdb/surrealdb:v3.1.3`, so the version is the same everywhere the suite
runs and nobody has to install a database to run it; `latest` would let a server
upgrade become a mystery failure in a test that never changed — deploys `graph-probe`
linked to `knowledge-graph` on a real fleet, and asserts that a node written
through `wasi:http` comes back with a slash-bearing id intact, that one hop out
finds the symbol, that one hop back finds the file, and that an edge nobody has
drawn reads as empty rather than as a failure. It skips loudly when Docker cannot start
the database; a skipped test that says so is honest, one that passes because it
did nothing is not. `docker compose --profile graph up -d` runs the same image
for development.

## What this does not do yet

- **No retention.** Nothing prunes a graph, and a loop that explores forever
  writes forever. Still true, and now true of three tables rather than one.
- **One database for the deployment.** The namespace and database are config, so
  two environments of the same app share a graph unless their config differs.
  Whether a fork should inherit its parent's memory or start blank is a real
  question and this ADR does not answer it.
- ~~**No embedding or similarity.**~~ → **ADR-0084**: `knowledge:memory` reaches
  SurrealDB's KNN through `query`, fuses it with `search:index`, and the HNSW
  index's `DIMENSION` turned out to be the model-drift guard this ADR wanted.
- **The database is not part of the platform.** It is an external service on an
  allow-list. Nothing deploys it, backs it up, or notices when it is gone.
