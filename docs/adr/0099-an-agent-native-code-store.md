# ADR-0099 — An agent-native code store: an oplog over a commutative, symbol-level patch graph

*Git's unit of change is a line in a snapshot, and its unit of concurrency is a lock
file. A swarm of agents editing one component hits both at once.*

**Status: served.** Step one — the contract (`wit/vcs/vcs.wit`) and the crate
layout (`crates/holon-vcs`) — is in. Step two — the engine, in-memory backends,
and the NATS JetStream + SurrealDB adapters, with every scenario run against
both — is in; its semantics are written down under *What step two decided*.
Step three — crash consistency (write-ahead intents, `verify`, `repair`), an
exact `commuted`, name reservations, explicit positions, and indexed SurrealDB
lookups — is in, under *Step three: consistency and correctness*; where it
changes a step-two rule, step three wins. Symbol extraction — splitting real Rust
and WIT files into symbols and back byte for byte (`crates/holon-vcs/src/extract.rs`)
— is in too, on the step-three engine (explicit positions, exact `commuted`),
under *Extraction*. And the WIT world is served: `comp-vcs`, a native daemon
over the engine, behind the `vcs-store` component, with `vcs-gateway` putting it
on HTTP for agents (`apps/vcs.toml`), end to end against real NATS and
SurrealDB, crashes included — under *The service*.

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
records is future work. *(Superseded by step three: the op is now logged
before the CAS, and every crash point is recoverable.)*

## Step three: consistency and correctness

The long form is in `crates/holon-vcs/src/{recovery,engine,order}.rs`; this is
the short one.

### The write order

Pointers, oplog and graph still share no transaction. Every op — apply,
conflict, resolve, revert — now writes them in one order
(`recovery::Engine::execute`):

1. **Intent.** Append the op to the log as `pending`. Besides the contract's
   `op-entry` it carries, per pointer move, a *guard*: the revision its CAS
   expects and the op the value it replaces names; and every record the op
   introduces — patch records, conflict records, conflict effects.
2. **Write-ahead graph.** Patch records (`pending`), the symbol's index entry,
   conflict records (uncommitted, `opened-at: 0`, the op in their `pending`
   set).
3. **Claims.** Name pointers the op takes (see *Names*).
4. **Commit point: the tip CAS**, from the guarded revision to the new value.
5. **Finish.** Conflict effects, name releases, the symbol mirror, and — last —
   the mark on the patch the tip now names; then CAS the op `pending →
   committed`.

A lost CAS at 3 or 4 undoes the op's claims, abandons its uncommitted conflicts
and CASes it `pending → aborted`; the call re-reads and retries as a new op.
Two changes make that recoverable from any point:

- **A pointer value names the op that wrote it** (`<hash>@<op>` in KV). Each
  guard names the op before, and an op reads a pointer before it is appended,
  so the writers of a pointer form a chain of strictly decreasing ids. Op `k`
  landed iff `k` is on the chain from the pointer's current value. No
  timestamps, no guessing: the pointer says.
- **Every graph write is idempotent and monotone in op id.** A patch's status,
  a symbol's mirror and a conflict's state each record the op that set them
  (`status_op`, `last_op`, `state_op`), and a write from an older op is ignored.
  Finishing an op may therefore be done by its writer, by a reader, or by
  repair — late, twice, or concurrently — without ever regressing a newer op's
  state.

### Why every crash point is recoverable

`settle(op)` decides any pending op:

| the op's tip pointer | means | settle |
|---|---|---|
| its chain contains the op | step 4 happened | **roll forward**: redo 2 and 5, mark `committed` |
| moved past the guarded revision, op not on the chain | step 4 can never happen (revisions never repeat) | **roll back**: undo claims still naming it, abandon its uncommitted conflicts, mark `aborted` |
| still at the guarded revision | in flight, or its writer died between 1 and 4 | within the lease (30 s, `with_lease_ms`): leave it. After: **fence** — CAS the pointer to the value it already holds, bumping the revision — then roll back. Fencing a writer that was only slow is safe: its CAS loses and it retries |

Crash points, then. *Before 1*: only content-addressed blobs were written.
*Between 1 and 4*: a pending op on an untouched pointer — aborted at once if
anything else moves the pointer, else after the lease; what it wrote ahead is a
pending patch nothing names and an uncommitted conflict abandoned with it.
*Between 4 and the end of 5*: a landed op — rolled forward by whoever next
reads the pointer, the oplog, or repair. The readers that matter settle before
they decide: reading a tip whose patch is not yet marked by the op the pointer
names finishes that op first (the mark is the last write of step 5, so a marked
tip is a finished tip), and reading a name reservation whose claim is pending
settles the claim. `oplog` and `oplog-head` stop at the first op still in
flight, so a reader paging through never skips an op that commits later; aborted
ops are skipped, so none is duplicated. An intent carries every record it adds,
so the graph can be rebuilt from the log and the blobs: `repair` re-finishes
every committed op in log order.

**`verify(workspace)`** checks, changing nothing: every pointer any op ever
moved is explained by a committed op whose move wrote exactly that value
(`unexplained-pointer`); no op is pending past the lease (`stale-pending`,
saying whether it landed); every tip's patch record exists (`missing-patch`)
and its mirror is current (`stale-mirror`); every conflict's sides exist
(`dangling-conflict`); every open conflict is committed and against its
symbol's tip (`orphan-conflict`); every live symbol's name is reserved for it
and no reservation is held for a symbol not called that (`stale-name`).
**`repair(workspace)`** settles every pending op (fencing those past the lease),
re-finishes every committed op and re-undoes every aborted one, and verifies
again; its report lists what it rolled forward, aborted, fixed, and what
remains (nothing, unless something outside the engine wrote a store).

Tested by a wrapper over all four stores that kills the "process" before its
k-th write, for every k, for eleven operations: replace, create, placed create,
rename (claim + tip + release), delete (tip + release), a conflict (record +
no-op CAS), move, resolve with a sibling re-pointed, resolve-by-delete, revert
of a replace, revert of a rename (three pointers) — 110 crash points. After
each: repair, then `verify` is clean, nothing is pending, every open conflict is
committed and against its tip; retrying the operation lands it exactly once
(`duplicate` if the crashed attempt had landed); and the workspace keeps
working. The same 110 again with the retry *before* repair, so the readers'
own settling is what recovers. Both in memory and against NATS + SurrealDB.
Losing the whole graph (a fresh database) and repairing reproduces every
query, conflict list and oplog exactly.

Two bugs the injection found, both fixed: a crash right after a conflict's
no-op CAS left the tip patch looking finished, and the retry opened the conflict
twice (a tip written by a different op than the one that landed its patch is now
settled first); and a crashed resolution was reported as a CAS race because the
conflict was read before its tip was settled.

### `commuted`, exactly

`patch-request` gains `read-at: option<op-id>`: the oplog position the agent's
view reflects. Every `symbol-view` carries `as-of` — the *settled head*, read
before the view — and `oplog-head` returns it directly. The settled head is the
highest id at and below which every op is committed or aborted, so a view
`as-of` n reflects every op ≤ n.

`commuted` iff `parent` was the tip AND at least one other op after `read-at`
that had landed by this patch's commit point moved the tip of another symbol of
the same component; `commuted-with` lists them. Conflict-only ops and reverts do
not count, as before. "Had landed" is checked just after the commit point over
the whole log after `read-at`, not just ids below this op's: ids are assigned
when an op is logged, so an op logged after this one may have landed before it.
Of two racing edits from one view, the one that lands second always lists the
other, and both may list each other — which is the truth: neither agent saw the
other's edit. With `read-at` a create can commute too. Without `read-at` the
step-two rule stands, documented as approximate: measured from the op that
landed `parent`, it also lists edits the agent may have read after fetching
`parent`, and a create never commutes. Pinned by `commuted_with_is_exact`.

### Names

A live symbol id is reserved by a name pointer `ws/<w>/name/<sha256 of the
id>` holding the symbol key that has it. `create` claims it; `rename` claims the
new name and releases the old; `delete`, and a resolution that deletes, release
it; a revert moves them back. Claims happen before the tip CAS and are undone if
it loses; releases after it. A reservation whose holder is no longer called that
(its release is late or was lost) is stale and may be claimed over — but only
once the claim that made it is known committed *and* the holder's tip, read
again after settling that claim, still does not match. (Reading the tip before
settling let two racing renames both win — the repeated race caught it; the 8-way race now
runs 10× per run in memory and against the live backends.)

**Decided: a new error, `name-taken(symbol-id)`, not
`concurrent-modification`.** A CAS race is "re-read and retry"; retrying a
rename onto a taken name can never succeed, so it must say so. Of N creates and
renames racing for one name, exactly one lands and the rest get `name-taken` —
except two `create`s of the *same* symbol id, which are two versions of one
symbol: the loser is a conflict (or a duplicate), as in step two. A create is
`name-taken` when the holder got the name by a rename (its key is not one the id
would be created at). Renaming onto a live name, previously `invalid`, is now
`name-taken`. A retry of a rename that already landed is a `duplicate` (found
through its parent, since the old name no longer finds it). Tested:
`rename_race_one_winner` (8 renames, one name), `create_vs_rename_race` (20
rounds; both sides win some), `names_release_and_reuse`.

### Positions

Step two laid a file out in creation order, and a symbol kept its place for
life, so an item inserted between two existing ones had nowhere to go. Now:

- A file is its live symbols sorted by **order key**, ties broken by **symbol
  key** (a SHA-256, so every node agrees). An order key is a string of decimal
  digits not ending in `0`, compared as bytes — a fraction — so between any two
  there is another (`order::between`), and inserting never renumbers anything.
- **`patch-request.position: option<placement>`** places a `create`.
  `placement` is `first | last | after(symbol-id) | before(symbol-id)`; the
  symbol named must be live, in the same component and path (`invalid`
  otherwise, `symbol-not-found` if it is not live), and not the symbol itself.
  The placement resolves, when the patch lands, to a key strictly between the
  two neighbours it names, among the file's *other* live symbols. `position` on
  anything but a `create` is `invalid`.
- **`transformation.move(placement)`** re-places an existing symbol: a patch
  like any other (content, edges and binding kept; two concurrent moves of one
  symbol conflict; revertable).
- **`none` appends**, and keeps step two's behaviour exactly: an unplaced
  symbol's key is derived from the op that first created it, `{op:020}5`, and
  every placed key is kept below the derived key of the next op id, so an
  unplaced create lands after everything placed or created before it.
- `replace`, `rename`, `delete` and recreate-after-delete keep the place; a
  resolution keeps `left`'s.
- **Concurrent inserts at one spot** compute the same key from the same view.
  They are different symbols on different pointers: all land, none conflicts,
  and the tie goes by symbol key, identically everywhere. A later insert
  "between" two tied symbols joins the tie. Tested with 8 agents inserting
  after one symbol at once (`concurrent_inserts_at_one_spot`, 10× in memory):
  all land between their neighbours, and the file is exactly (order key, symbol
  key) order.
- The placement is hashed **as requested** (`after(x)`), not as resolved, so a
  retried placed create is a `duplicate` even if the file changed in between.
  Unplaced patches hash as in step two.

This is what symbol extraction's ingest now does (see *Extraction*): a new
item right after `p` in the file is `create` with `position: after(p)` (`first`
when it leads the file), an existing item whose place changed is
`move(after(p))`, unchanged items need nothing, and every patch carries
`read-at`. Mid-file inserts are no longer folded into a neighbour.

### SurrealDB: indexes, and a database per test run

The live suite slowed from 4 s to 8 s over runs on one server. The cause was
not the lookups the ADR worried about but the writes: `put_patch` deleted a
patch's old edges with `DELETE depends_on WHERE in = $p` — a scan of every
edge ever written, by every workspace — and `put_conflict` did the same on
`conflicts_with`. Every lookup now has an index, listed in `surreal.rs` with
the statement it serves: `symbol(ws, component, path, kind)`,
`symbol(ws, component)`, `conflict(ws, key, state)`, `conflict(ws, state)`,
`depends_on(in)`, `implements(in)`, `conflicts_with(ws, conflict)`; patches,
symbols, conflicts and symrefs by hash are record-id reads, and dependents a
graph-edge scan. `surreal_queries_use_indexes` runs `EXPLAIN` on every one and
fails on a table scan. The live suite uses one database per run.

Measured (`tests/bench.rs`, release build, SurrealDB 3.1.3 in memory, one
workspace of 5 000 symbols, 20 000 patches each with a `depends_on` edge, and
1 000 conflicts):

| | before | after |
|---|---|---|
| insert phase | ~540 s | 6.8 s |
| `put_patch` p50, first → last tenth of the data | 9.6 ms → 208 ms (21.7×) | 5.6 ms → 5.7 ms (1.00×) |
| `put_patch` p99, last tenth | 16 s | 37 ms |
| `put_conflict` p50 | 2.55 ms | 0.48 ms |
| point lookups (`symbols_named`, `patch`, `dependents`, `open_conflicts_for`, `symbol`) p99 | ≤ 1.3 ms | ≤ 1.4 ms |

The live suite no longer drifts between runs. Its 11 step-two tests take about
6 s, where they took 3.6 s on a fresh server: each op now writes more (the
intent, its finish, name pointers). The full live suite (22 tests, 220 crash
points) takes about 14.5 s.

SurrealDB reports a failed `BEGIN … COMMIT` block as "not executed due to a
failed transaction" on every statement but the one that failed; the adapter now
looks at all of them before deciding whether a failure is a retryable conflict.

### Contract changes

`patch-request` gained `read-at` and `position`; `transformation` gained
`move(placement)`; `symbol-view` gained `as-of`; `vcs-error` gained
`name-taken(symbol-id)`; `code-store` gained `oplog-head`, `verify` and
`repair`, with `consistency-report`, `repair-report` and `inconsistency`.
`oplog` now returns committed ops only. Still 0.1.0: unreleased.

### Still open

- A crashed writer's intent is presumed in flight until its lease expires
  (30 s). Until then it blocks `snapshot-export` (which retries, then reports
  `concurrent-modification`) and holds `oplog`/`oplog-head` at the op before
  it. Fencing earlier is always safe; the lease is a liveness knob.
- A lost CAS now leaves an aborted op in the log (op ids are no longer dense
  among committed ops; `oplog` skips them). Under an N-way race that is up to
  a few aborted intents per loser.
- Crash injection is at the store-trait level. The oplog's own two-key append
  (entry, then head) is below it; a lagging head was already walked past in
  step two.
- `verify` walks the whole log and `repair` re-finishes every committed op:
  fine for a repair tool, not for a hot path.

## Extraction

`extract(component, path, bytes)` cuts a file into symbols in file order;
`ingest_file` / `ingest_tree` turn an agent's edited copy into patches.

**The lossless rule.** The symbols' contents, concatenated in order, are the
file's bytes — comments, blank lines, attributes, doc comments, `//!` headers,
BOM, CRLF, a missing final newline. Every byte belongs to exactly one symbol:
leading trivia (comments, `///`, `#[attrs]` above an item) to the item below it;
the rest of an item's last line (`} // why`) to the item; whitespace after the
last item to the last item, a trailing comment to a `(tail)` symbol. Cuts are made
only just after a `\n`, so every symbol but a file's last ends in one — exactly
what the snapshot layout (a `\n` inserted only where a symbol lacks one) needs to
give the file back unchanged. Two items on one line are one symbol. `extract`
checks its own output against the rule and falls back to one `kind: file` symbol
rather than lose a byte.

**Granularity, Rust** (`syn` 3, `full`, spans from `proc-macro2`'s
`span-locations` fallback — pure Rust, builds and runs on wasm32-wasip2):
one symbol per top-level item — `fn`, `struct`/`union`, `enum`, `trait`, `impl`
(named `impl Order`, `impl Display for Order`), `const`, `static`, `type`,
`macro_rules! m` (`m`) and item macros (`bindings::export!`), `mod m;`,
`extern "C" {}`.
- **An impl block is the unit, not its methods.** The block is what carries
  `Self` and the trait. Two agents editing two methods of one impl therefore
  conflict; methods as symbols (head + methods + `(end)`, as for inline modules
  — positions now make that placeable) is the next refinement.
- **Inline modules recurse**: `mod tests { .. }` is `tests` (attributes, the
  `mod tests {` line, the module's leading `use`s), one symbol per inner item
  (`tests::adds_up`), and `tests::(end)` (the closing brace and trivia before it).
  Test modules are where concurrent agents collide most. A module whose braces
  share a line with its items is kept whole.
- **`use` declarations** before the first other item, with the BOM, shebang, inner
  attributes and `//!` docs, are one `(header)` symbol (kind `module`); a later run
  of `use`s is one `(use)` symbol. Grouped, not one per line, because a lone `use`
  line is not a meaningful unit to review or conflict on.
- A repeated name (`#[cfg]`-twinned `fn`s, two `impl Order` blocks) gets `~2`,
  `~3` in file order.

**Granularity, WIT**: a brace-matching splitter that knows `//`, `///` and nested
`/* */` — one symbol per `interface` (wit-interface) and `world` (wit-world), a
nested `package x:y { }` block as one module, `package`/`use` as `(header)`.
`wit-parser` is not used: its spans serve diagnostics and it resolves far more
than a splitter needs.

**Fallbacks**: a file `syn` cannot parse, unbalanced WIT, a file with no items
(only comments; empty), and every other extension are one `kind: file` symbol
named by the path. Non-UTF-8 bytes are stored as one blob.

**Edges, best effort.** `depends-on`: identifiers an item mentions that another
item of the same file defines, by simple name. `implements`: `impl Trait for X`
where `Trait` is in the file. `wit-binding`: `impl exports::ns::pkg::iface::Guest`
→ `ns:pkg/iface`, a bare `impl Guest` → `Guest`, a WIT interface/world → its
`ns:pkg/name` (no version), so the two spellings meet. Cross-file edges are not
computed yet.

**Ingest.** `ingest_file(engine, ws, component, path, bytes, agent, read_at)`
diffs the extracted symbols against the file's symbols AS OF `read_at` (the op
the agent read at; `None` = `oplog-head` now), not as of now: tips are replayed
from the committed ops at or below it (a pointer's writers form a chain of
increasing op ids, so the last one in id order is its value there), and the
file's order is theirs at that point — (order key, symbol key), as
`snapshot-export` lays it out. `read_point` is `oplog-head`, and every patch an
ingest sends carries `read-at` = the read point, so `commuted` is exact. An
unchanged symbol is no patch; a changed one is a `replace` whose parent is its
tip at the read point, so another agent's later edit to the same symbol comes
back as a conflict and an edit to a different symbol commutes — never a silent
overwrite of work the agent had not seen. Vanished symbols are `delete`d
first; a new item standing where a vanished one of the same kind stood, with
at least half its lines, is a `rename` (plus a `replace`). `ingest_tree` does a
set of files against one read point; with `prune`, a file the store has and the
set does not is deleted. Content changes only — an unchanged symbol whose
computed edges changed is not rewritten.

**Positions: every item is its own symbol, wherever it stands.** Until step
three a symbol kept its creation-order place for life, so an item inserted
between existing ones had no place of its own and ingest folded it into its
neighbour. That is gone (and with it `IngestReport::folded`): of the symbols
the file shares with the read point, the longest run whose order the file keeps
stays put; the rest of those are `move(after(p))` (`first` at the top), where
`p` is the item before it in the file; a new item is `create` with `position:
after(p)` (or `first`). Patches go in file order, so `p` has always just been
placed; `p` is the last preceding item live under its planned name (a failed
create is skipped over). A new item with no existing one after it is created
unplaced — `none` appends, which is the same place — because resolving an
explicit placement reads the component's other symbols, which would make
importing a whole file quadratic; a recreate of a deleted id is always placed,
since unplaced it would return to its old place. Two agents inserting different
new functions at the same spot of `idlist.rs` from one read point both land as
their own symbols, with no conflict: landing one after the other, the later
one's `after(insert)` resolves against the file as it is by then and goes first
(B's, then A's, exactly); racing, the file is one of the two orders, the one the
log determines (a tie by symbol key), and query and export agree. Moving two
functions of the file (one up, the last one up past most of the file) is exactly
two `move`s and exports byte for byte, and so does moving them back.

**Measured.** 307 files of this repository (every `.rs` of `crates/holon-vcs`,
`components/record-store/src`, `reconciler/src/bin/media.rs`, every `.wit` under
`wit/` and `components/*/wit`; 1.5 MB, 1599 symbols, none kept whole): extract →
ingest → `snapshot-export` gives 0 byte differences and the snapshot's `git-tree`
equals `git write-tree` of the originals; re-ingesting the unchanged tree writes
no patch. Extraction alone over every `.rs` file of the repo's workspaces:
lossless, none left unsplit. On a real file (`record-store`'s `idlist.rs`), two
agents from one read point editing two functions concurrently both land (one
`commuted`) and the export has both edits; editing the same function opens one
conflict with both versions verbatim, and the resolution exports correctly. All
of it — scenarios and corpus — also runs against live NATS JetStream +
SurrealDB (`tests/extract.rs`, env-gated like `tests/live.rs`, one SurrealDB
database per run).

**Still open.**
- **Inside a tie, placement cannot order.** Two symbols that tied (placed at one
  spot from one view) are ordered by symbol key; `after(x)` of the first of them
  joins the tie too, so an item an agent inserts between two tied symbols, or a
  move within the tie, lands by symbol key, not where the agent's file has it.
  The bytes of what landed are exact; the order within the tie is the store's.
  Breaking it needs an engine placement relative to the agent's view.
- Every placed create or move resolves against the component's whole symbol
  list (`component_order`): linear per patch, fine for edits, the reason whole-
  file imports append.
- Cross-file edges; methods as symbols.

## The service

`docs/apps/VCS.md` is the long form; `components/vcs-store/CONTRACT.md` is the wire.

```
agent → comp-host (vcs-gateway ⊕ vcs-store) → comp-vcs → NATS JetStream + SurrealDB
```

- **`comp-vcs`** (`reconciler/src/bin/vcs.rs`) is the engine over its two native
  adapters and nothing more: one HTTP route per WIT function
  (`POST /v1/<function>`), startup repair, and an allow-list for `materialize`.
  ADR-0095's three questions are answered in its header: it needs sockets a
  guest does not have; every decision stays in the engine, which also builds for
  wasm32-wasip2; and the contract stays WIT.
- **`vcs-store`** exports `holon:vcs/code-store` and `holon:vcs/files`, each call
  one request to the daemon (`vcs-url`, `vcs-token`); **`vcs-gateway`** imports
  them and serves the same routes over `wasi:http`. The JSON is defined once
  (`holon_vcs::wire`) and used by all three: the model records as they already
  serialise (kebab-case, like the WIT), request envelopes for multi-argument
  functions, bytes as base64. Both components convert between their own
  bindings and the model with one source file (`vcs-store/src/witconv.rs`,
  included by path — each crate's bindings are its own types, ADR-0095).
- **Errors** travel as `{error, detail, message}`: the `vcs-error` case and its
  payload, with a status by kind (409 race/conflict/name, 404, 400, 503). A
  client decides by `error`; anything it cannot read is `storage-error`, the case
  a caller may retry.
- **Startup repair.** Before listening, `comp-vcs` lists the workspaces in its
  oplog bucket (`nats::workspaces`, which needs the new `NatsKv::keys` and
  `store::unescape`) and repairs each: what a dead predecessor left landed is
  rolled forward, what cannot land any more is aborted, and an op inside the
  lease stays in flight.
- **Crash injection is a process abort.** `--crash-after-step` /
  `--crash-before-step <step>[:<n>]` (with `--i-know-this-is-a-test`) wrap the
  four stores and `abort()` at the n-th write of a step — `blob`, `intent`,
  `graph`, `claim`, `tip`, `finish`, `release`, `commit` — so the e2e suite
  kills a real process mid-write, restarts it, and checks the result through
  the gateway. Step two's injection was a fuse inside one process; this one
  loses everything in memory, as a crash does.

**Contract change** (still 0.1.0, unreleased): a second interface,
`holon:vcs/files` — `ingest-file`, `ingest-tree`, `materialize`, `read-blob` —
and both worlds carry it. Ingest and materialize are what an agent with a
working copy needs, and `read-blob` is the only way to read a symbol whose
content comes back as a `blob` (over 64 KiB). `materialize` writes on the
serving side, under its allow-list; outside it the caller gets
`invalid("not-permitted: …")` — `vcs-error` gained no case.

**Measured** (`reconciler/tests/e2e_vcs.rs`, `bash e2e/vcs.sh`): eight scenarios
through the gateway — the two-agent commute and conflict, ten crash points
(each a real abort and restart), exact vs approximate `commuted`, an eight-way
rename race, record-store's real sources ingested and materialized byte for
byte with git tree ids checked against `git`, concurrent ingests (two functions,
one function, two inserts at one spot), a move, and the lease holding export —
in about 20 s, five runs in a row green.

**Still open.** The gateway has no authentication (tailnet only); a daemon
`--token` is the only credential. SurrealDB's default port is also
`comp-fswatch`'s in `apps/fs-watcher.toml`.

## Consequences

- A merge never needs a working copy, so any node can do one.
- Symbol extraction (splitting a file into symbols) is a separate concern from the
  store; a whole file is also a symbol (`kind: file`), so nothing is unrepresentable
  for a language without a splitter (see *Extraction*).
- Two edits to one symbol that do not *textually* overlap still conflict. That is
  deliberate: within one function, "disjoint lines" is exactly the adjacency argument
  this ADR rejects. Finer granularity (statements) can come later without changing
  the contract.
