# Embed crdt:merge in-process via jco

The `crdt:merge` component running **inside the Node process** — no wasmCloud,
no NATS, no host shims. Pure compute: state in, merged state / value out. `jco
transpile` turns `crdt.wasm` into JS; this example calls its exported `merger`
interface directly.

**Conflict-free replicated data types (state-based CvRDTs).** The class no other
showcase covers: many replicas mutate the same value with no central lock,
exchange state out of order, and still converge to the same result. The whole
guarantee rides on one operation — `merge` computes a least-upper-bound, so it
is **commutative, associative, and idempotent**. Merge order and delivery order
don't matter. The test proves it: 40 random permutations of the same states all
fold to byte-identical output.

State is an opaque, self-describing JSON string with a `"type"` tag; `merge` and
`value` dispatch on it. Four canonical types:

- **`lww`** — last-writer-wins register. `lwwNew` / `lwwSet(state, v, ts,
  replica)`. Higher `(timestamp, replica)` wins.
- **`pn`** — PN-counter. `counterNew` / `counterAdd(state, replica, delta)`.
  Value = increments − decrements; merge is per-replica max.
- **`orset`** — observed-remove set. `orsetNew` / `orsetAdd(state, el, tag)` /
  `orsetRemove(state, el)`. A concurrent add beats a concurrent remove
  (**add wins**).
- **`lwwmap`** — per-key LWW map (with tombstones). `lwwmapNew` /
  `lwwmapSet(state, key, v, ts, replica)` / `lwwmapRemove(...)`. This is what
  `scribe` (the collaborative editor) uses for per-field convergence.
- **`rga`** — text **sequence** (replicated growable array). `rgaNew` /
  `rgaInsert(state, index, text, idBase)` / `rgaDelete(state, index, count)`.
  Concurrent inserts at the same position **interleave deterministically** —
  two people typing into one string, neither lost. `idBase` must be unique +
  sortable.

Timestamps and replica ids are caller-supplied, so results are deterministic —
no wall clock inside the component.

```
crdt.wasm                # the built component (pure compute, standard WASI only)
test/
  crdt.test.ts           # per-type behavior + the convergence property
gen/                     # transpile output (gitignored) -> gen/crdt.js
```

## Run

```bash
npm install
npm run transpile        # crdt.wasm -> gen/
npm test                 # behavioral + property (convergence) checks
```

`jco transpile crdt.wasm -o gen` — no `--map` flags, since the component imports
only standard WASI interfaces and computes in-process.
