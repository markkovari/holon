# ADR-0099 — An agent-native code store: an oplog over a commutative, symbol-level patch graph

*Git's unit of change is a line in a snapshot, and its unit of concurrency is a lock
file. A swarm of agents editing one component hits both at once.*

**Status: proposed.** Step one — the contract (`wit/vcs/vcs.wit`) and the crate
layout (`crates/holon-vcs`) — is in; the engine and its adapters follow.

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
     JetStream **ObjectStore**, named `blob:<sha256>`.
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

## Consequences

- A merge never needs a working copy, so any node can do one.
- Symbol extraction (splitting a file into symbols) is a separate concern from the
  store; a whole file is also a symbol (`kind: file`), so nothing is unrepresentable
  before a parser exists for it.
- Two edits to one symbol that do not *textually* overlap still conflict. That is
  deliberate: within one function, "disjoint lines" is exactly the adjacency argument
  this ADR rejects. Finer granularity (statements) can come later without changing
  the contract.
