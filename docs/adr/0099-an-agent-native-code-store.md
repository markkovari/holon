# ADR-0099 — An agent-native code store: an oplog over a commutative, symbol-level patch graph

*Git's unit of change is a line in a snapshot, and its unit of concurrency is a lock
file. A swarm of agents editing one component hits both at once.*

**Status: proposed.** Step one — the contract (`wit/vcs/vcs.wit`) and the crate
layout (`crates/holon-vcs`) — is in. Step two — the engine, in-memory backends,
and the NATS JetStream + SurrealDB adapters, with every scenario run against
both — is in; its semantics are written down under *What step two decided*.
What is left: a component or daemon serving the WIT world over the engine, and
symbol extraction (splitting source files into symbols).

## Why not git

Autonomous agents working in parallel on one repository fail on git in three ways
that have nothing to do with the code they wrote:

- **Locks.** `.git/index.lock` serialises every writer on a checkout; a second agent
  gets an error, not a queue.
- **Non-commutative merges.** Line-diff three-way merge conflicts on *adjacency*:
  two agents editing two different functions that happen to sit next to each other
  collide. The same two edits in the other order may merge differently.
- **State an LLM cannot hold.** Rebase, detached HEAD, a half-finished merge — each is
  a mode the next command behaves differently in, and a model driving git by shell
  loses track of which mode it is in.

## The decision

1. **The unit of change is a symbol.** An edit names the symbol it touches (component,
   file, item path, kind) and the patch it was computed from. Edits to different
   symbols commute and both land in any order. Edits to the same symbol from the same
   parent are not an error: they become a `conflict { base, left, right }` record with
   both sides kept verbatim, which a resolver agent reads and settles with
   `resolve-conflict`. Jujutsu's "conflicts are data" and Pijul's commutation, at
   symbol granularity.
2. **Every edit is an automatic commit.** No staging area, no index. Each mutation is
   an entry in a linear per-workspace operation log carrying the pointer moves needed
   to undo it; `revert-op` appends the inverse.
3. **Storage is split by mutability.**
   - Immutable bytes — source blobs, AST payloads, built `.wasm` — go in a NATS
     JetStream **ObjectStore**, each object named by its SHA-256.
   - Mutable pointers — workspace heads, symbol tips — go in NATS **KV**, written only
     by compare-and-set against the revision read (the lesson of ADR-0065 and #284:
     a guard that re-reads before writing is not a guard).
   - The structure — `symbol`, `patch`, `depends_on` / `implements` /
     `conflicts_with` — goes in **SurrealDB**, the graph store ADR-0080 already runs.
4. **The contract is WIT** (`holon:vcs/code-store`), with typed records and variants
   and an error variant that tells a CAS race (`concurrent-modification`: retry) from
   an agent disagreement (a conflict: resolve) from a missing symbol from a store
   outage.

## Where things go, and why not where the brief first put them

- **`wit/vcs/vcs.wit`, not `wit/vcs.wit`.** A WIT directory is one package, and
  `wit/` is already `auth:identity`, which dozens of components depend on by path.
  A second package beside it would break every one of them; `wit/ui-assets/` is the
  precedent for a sibling package.
- **One crate, two layers, split by a feature.** `async-nats` and `surrealdb` need
  sockets and a runtime, so they cannot run inside a `wasm32-wasip2` component
  (ADR-0095). The crate's default build is the pure engine — patch algebra, conflict
  detection, oplog — written against storage *traits*, with no I/O, and it builds for
  both targets. `--features native` adds the NATS and SurrealDB adapters behind those
  traits. The WIT world is then served either by a thin component in front of a
  native `comp-vcs` daemon (the `image-optimizer` → `comp-imageopt` shape) or by the
  engine in-process over host capabilities.
- **`crates/` is a new workspace root**, like `lattice/`: a library for both targets
  that is neither a component nor a binary.
- **`snapshot-export` returns a manifest, not a directory.** A component has no
  filesystem (ADR-0023). It returns `(path, blob, executable)` entries and the git tree
  id — the SHA-1 `vgit:store` would name it — and the host materialises it.

## What step two decided

The contract left a handful of things open. They are settled in the engine
(`crates/holon-vcs/src/{patch,engine}.rs` carry the long form); this is the
short one.

**Names.** A patch hash is SHA-256 over a canonical, length-prefixed encoding of
(symbol key, sorted parents, change), where content is always a blob hash —
inline text is stored first, so `inline(x)` and `blob(sha256(x))` are one patch.
Agent, message and time are metadata, not hashed: the same edit by two agents is
a `duplicate`. A symbol key is the SHA-256 of the id it was created as (plus a
probe index, used only when a name is reused after its holder was renamed), so
two agents racing to create one symbol race on one pointer. A conflict id is the
SHA-256 of its two side hashes, sorted.

**Outcomes.**
- `applied`: `parent` was the symbol's tip and the tip moved to the new patch.
- `commuted`: the same, AND at least one op strictly between the op that landed
  `parent` and this patch's op moved the tip of another symbol in the same
  component; `commuted-with` lists them. Measured from `parent`'s op because the
  request carries nothing else, so it is "what this edit was reordered past, at
  most", not "what the agent had not seen". Computed after the op is appended,
  over the log before it: of two racing edits to two symbols, the later op
  reports the earlier, and the earlier reports `applied`. A `create` never
  commutes.
- `conflicted`: the tip moved on *this* symbol since `parent`. `left` = the
  current tip, `right` = the new patch, `base` = `parent`; the tip stays at
  `left`. A retry of the same stale edit returns the same conflict.
- `duplicate`: the patch is the tip, or (for a stale parent) it already landed.

**Open conflicts** refuse an edit that would move the tip (`parent` == tip) with
`unresolved-conflict`, and export of the component. They do NOT refuse a stale
edit: it becomes another conflict against the tip. So an **N-way race** from one
parent ends with one `applied` and N-1 conflicts, each `(winner, loser_i)`, and
every patch recorded — measured, 8 agents, in memory 25x per run and over NATS +
SurrealDB 10x per run. `resolve-conflict` moves the tip from `left` to a
resolution with parents `[left, right]`, and in the same op re-points every
other open conflict on the symbol from `(left, x)` to `(resolution, x)`, so a
resolver working through a race always merges into the current tip.

**Why a conflict also compare-and-sets the tip** (to the value it already has):
the conflict record is written before that CAS and an applier reads open
conflicts after reading the tip, so either the applier sees the conflict, or
both CAS against one revision and exactly one lands. Without it an edit could
land on a tip that a conflict opened an instant earlier names as its `left`.
Tested by stopping a conflict between its record and its CAS
(`tests/common::cas_retry`, case 5).

**Revert** refuses with `concurrent-modification`, changing nothing, if any
pointer the op moved was touched by a later op (a conflict's no-op move counts:
that conflict names the value) or does not hold the op's `after` now. It appends
`revert(op)` with the inverse moves and inverts the conflict state changes the op
recorded: reverting an apply that only opened a conflict abandons it; reverting
a resolve re-opens it and undoes the re-pointing; reverting a revert re-applies.
Every op the engine writes moves one pointer; for many, all are validated first,
and a lost CAS midway rolls back those already moved (best effort — the one
window left is reported, not hidden).

**Delete, rename, layout.** A delete leaves its patch as the tip (the pointer is
never deleted: that would reset its revision — #284's ABA); a later `create`
builds on it. A rename keeps the key and moves the name; the old name is free for
a new symbol. A snapshot lays each file out in creation order (the op that first
created each symbol; rename and recreate keep the place), content verbatim, with
a newline inserted between two symbols only where the first lacks one; its
`git-tree` is checked against `git write-tree` in the tests.

**Contract changes** (0.1.0 was unreleased): `patch-request` gained `implements`
and `wit-binding`, without which the `symbol-view` fields of those names could
never be set; `resolution-request` gained `workspace`, since every other call is
scoped by one and a conflict id is not unique across workspaces; the `conflict`
doc said the tip stays at `base`, which is wrong once one side has landed — it
stays at `left`.

**Adapters.** Blobs: JetStream ObjectStore, object name = SHA-256; single-chunk
objects are read with one direct get, because `ObjectStore::get` creates an
ordered consumer per read and the live suite exhausted the stream's consumer
limit within a few runs. Pointers and the oplog: JetStream KV — `create` for
"must not exist", `update(key, value, revision)` for CAS; the oplog appends by
`create`-ing the next entry key (one winner per id) and advancing a head by CAS.
Keys are escaped (`=XX` for anything outside `[A-Za-z0-9_-]`) and hashed past
256 bytes. Graph: SurrealDB 3, tables `symbol`/`patch`/`conflict`/`symref`/
`op_effect`, `RELATE`d `depends_on`/`implements`/`conflicts_with`, every value a
bound parameter and every record id a hash; optimistic-transaction conflicts are
retried (a cancelled transaction changed nothing).

**Not atomic.** Pointers, oplog and graph are three stores. A crash after a tip
CAS and before its op append leaves a tip the log does not explain; the graph is
an index the engine re-verifies against pointers, so a lagging mirror costs a
miss, never a wrong write. A repair job that rebuilds from pointers and patch
records is future work.

## Consequences

- A merge never needs a working copy, so any node can do one.
- Symbol extraction (splitting a file into symbols) is a separate concern from the
  store; a whole file is also a symbol (`kind: file`), so nothing is unrepresentable
  before a parser exists for it.
- Two edits to one symbol that do not *textually* overlap still conflict. That is
  deliberate: within one function, "disjoint lines" is exactly the adjacency argument
  this ADR rejects. Finer granularity (statements) can come later without changing
  the contract.
