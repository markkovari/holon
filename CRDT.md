# crdt — conflict-free convergence (the primitive `scribe` builds on)

A **state-based CRDT** capability (`crdt:merge`): many replicas mutate the same
value with **no central lock**, exchange state **out of order**, and still end
up **identical**. Chosen because it's the one axis none of the other showcases
touch — everything else is request/response or single-writer; even `pulse` only
*pushes*, it doesn't *merge*. This is the convergence class, and it's the
primitive the collaborative editor (`scribe`) is blocked on.

It's a pure-compute component — state in, merged state / value out. No host
imports, no stored state (the caller owns persistence), no wall clock inside
(timestamps + replica ids are caller-supplied, so results are deterministic).

![Three replicas of one document edit offline and diverge — Alice renames the title and tags it "urgent", Bob renames it later and moves it to review, Carol removes "urgent" and tags "q3" — then a SYNC merges all three and every pane converges to the identical result: title "Design proposal" (last-writer-wins), ♥ 6 (summed), tags backend/q3/urgent where "urgent" survived Carol's remove because Alice's concurrent add wins](docs/media/crdt.gif)

## The one property that matters

`merge` computes a **least-upper-bound** in a join-semilattice, which makes it
**commutative, associative, and idempotent**:

```
merge(a, b) == merge(b, a)                 // order of peers doesn't matter
merge(merge(a, b), c) == merge(a, merge(b, c))   // grouping doesn't matter
merge(a, a) == a                            // re-delivery is harmless
```

So replicas converge regardless of the order state arrives in. That's the whole
game, and it's **property-tested both sides**: the Rust `cargo test` folds every
permutation of a set of states and asserts identical bytes; the jco e2e does the
same over 40 random permutations against the actual compiled wasm. Output is
canonical (sorted keys, sorted sets), so *equal states are byte-equal* — you can
check convergence with `==`.

## Five types, one per CRDT family

| type | family | value | conflict rule |
|---|---|---|---|
| `lww` | register | the stored JSON value | higher `(timestamp, replica)` wins |
| `pn` | counter | Σ increments − Σ decrements | per-replica max (both directions) |
| `orset` | set | present elements | **add wins** over a concurrent remove |
| `lwwmap` | map | live `key → value` | per-key LWW (with tombstones) |
| `rga` | **sequence** | the text | concurrent inserts **interleave**, never clobber |

`lwwmap` is what `scribe` uses for the document: a map of fields, each a
last-writer-wins register, so two people editing different fields never conflict
and two editing the same field resolve deterministically.

`rga` (a replicated growable array) goes further — it's a **text sequence**
where two people typing into the *same* field **interleave** instead of one
winning: each character is an element anchored after another, concurrent inserts
at the same spot order deterministically by id, and delete is a tombstone so an
edit racing a delete both survive. This is the upgrade path from `lwwmap`'s
per-field LWW to true concurrent character editing.

## The demo (what the gif shows)

Three replicas fork from the same document, then edit **offline** (partitioned):

- **Alice** renames the title, +2 ♥, tags `urgent`.
- **Bob** renames the title *later* (higher timestamp), moves it to `review`, +3 ♥, tags `backend`.
- **Carol** +1 ♥, adds then **removes** `urgent`, tags `q3`.

`SYNC` merges all three. Every pane converges to the same state:

- **title** → `"Design proposal"` — Bob's write has the later stamp (**LWW**).
- **likes** → `6` — `2 + 3 + 1`, the **PN-counter** sum.
- **tags** → `backend, q3, urgent` — union; `urgent` **survives** Carol's remove
  because Alice added it concurrently with a tag Carol never saw (**add wins**).

Every value on screen is produced by the real Rust `crdt.wasm`; the recorder
just lays the merged states across three panes.

## Run it

```bash
cargo test -p crdt                     # Rust: convergence property + per-type rules
cd examples/jco-crdt && npm test       # jco e2e: same, against the compiled wasm
```

Regenerate the gif (`tools/screencast/`): `cd examples/jco-crdt && npm run
transpile`, then `node tools/screencast/crdt.mjs` and `bash to-gif.sh
videos/crdt/*.webm ../../docs/media/crdt.gif 900 12`.

## Next

`scribe` — a collaborative document editor: `lwwmap` per field for convergence +
`pulse`'s SSE spine to push merged state to every open editor. This component is
the piece it was waiting on.
