# vcs — the agent-native code store, served

A code store for a swarm of agents editing one codebase at once, where git
fails them: there is no lock to wait on, two edits to two different functions
both land whatever order they arrive in, and two edits to the SAME function
become a conflict record with both versions kept — data a resolver agent reads
and settles — rather than an error or a half-finished merge. Every edit is an
automatic commit in an operation log; any op can be reverted; a crash at any
point of a write is recovered by the next call that meets it.
[ADR-0099](../adr/0099-an-agent-native-code-store.md) is the design; this is the
service that puts it on the network, and
[ADR-0134](../adr/0134-the-code-store-is-a-component.md) why it is one component.

## How it fits together

```
agent ──HTTP──▶ comp-host ┌ vcs-gateway ──holon:vcs──▶ vcs-store ┐ ──comp:store/cas──▶ the app's keyvalue
                          └──────────── one composed component ──┘    (blobs, pointers, oplog, graph)
```

| piece | what it does |
|---|---|
| `wit/vcs/vcs.wit` | the contract: `holon:vcs/code-store` (apply, resolve, revert, query, export, conflicts, oplog, verify, repair) and `holon:vcs/files` (ingest real files, materialize a snapshot, read a blob) |
| `crates/holon-vcs` | the engine — every decision: commutation, conflicts, names, positions, the write order and its recovery, symbol extraction — and its stores over a compare-and-set keyvalue (`KvBlobs`, `KvPointers`, `KvOpLog`, `KvGraph`). Pure; builds for wasm32-wasip2 and for the host |
| `vcs-store` (`components/vcs-store`) | exports the contract and runs the engine in-process over the app's keyvalue; `materialize` writes under the host's `--dir` ([CONTRACT](../../components/vcs-store/CONTRACT.md)) |
| `vcs-gateway` (`components/vcs-gateway`) | imports the contract and serves it as `POST /v1/<function>` for agents, `comp-agentd` and tests |
| `apps/vcs.toml` | the app: gateway ⊕ store on `:3942`, `kv = "sqlite"`, `dirs = ["/var/lib/vcs/checkouts"]`, `mem_cap_mb = 256` |

The JSON is defined once (`crates/holon-vcs/src/wire.rs`) and answered by one
dispatch (`holon_vcs::serve`); refusals are `{error, detail, message}` with the
`vcs-error` case, its payload, and a status by kind (409 for a race or a
conflict, 404, 400, 503 for storage).

## Run it locally

```
cargo xtask build
cargo xtask host vcs              # comp-host on :3942, the store in a sqlite file
```

It makes `/var/lib/vcs/checkouts` for `materialize`, or says the `sudo` that would. With
`kv = "memory"` nothing is kept across a restart.

An agent's loop, by hand:

```
H=$(curl -s -XPOST localhost:3942/v1/oplog-head -d '{"workspace":"w"}')
curl -s -XPOST localhost:3942/v1/ingest-file -d "{\"workspace\":\"w\",\"component\":\"shop\",
  \"file\":{\"path\":\"src/orders.rs\",\"content\":\"$(base64 < src/orders.rs | tr -d "\\n")\"},
  \"by\":{\"id\":\"agent-1\"},\"read-at\":$H}"
curl -s -XPOST localhost:3942/v1/list-conflicts -d '{"workspace":"w","state":"open"}'
curl -s -XPOST localhost:3942/v1/materialize -d '{"workspace":"w","component":"shop","dest":"/var/lib/vcs/checkouts/shop"}'
```

`ingest-file` diffs the agent's copy against the component *as of the agent's
read point* and sends only what changed — a function edited, inserted, moved,
renamed or deleted — so another agent's later edit to the same function comes
back as a conflict, never as a silent overwrite.

## The e2e suite

```
bash e2e/vcs.sh              # all scenarios, once
RUNS=5 bash e2e/vcs.sh       # five times over
bash e2e/vcs.sh f_real       # a name filter, passed to the test binary
```

`reconciler/tests/vcs_app.rs`, in CI with the other built-artifact suites. Each
scenario starts its own `comp-host` with the composed gateway, its own sqlite
file and its own `--dir`, and talks to nothing but the gateway:

| | scenario |
|---|---|
| A | two agents edit `compute_total` and `validate_order` — adjacent functions of one file — at once from one read point: both land, the second `commuted` naming the other's patch, and the export has both |
| B | two agents edit `compute_total` at once: one lands, one is `conflicted` with both versions verbatim; export and materialize are refused `unresolved-conflict`; a resolver merges; the materialized directory's `git write-tree` equals the snapshot's `git-tree` |
| D | `commuted` with `read-at` is exact (an edit read after is `applied`, one read before commutes past exactly it); without `read-at` it over-reports, as documented |
| E | eight renames to one name at once: one lands, seven `name-taken` naming it |
| F | `components/record-store`'s real files (`.rs`, `.wit`, `Cargo.toml`) ingested, exported and materialized byte for byte, git tree id checked against `git`; re-ingest writes nothing; then two agents ingest edited copies of `idlist.rs` at once — two neighbouring functions (both land), the same function (one conflict, resolved), two inserts at one spot (both land, no conflict) |
| G | a function moved by ingest is one `move`, and the export is exact; moved back, exact again |

A crash at each step of the write order — after the blob, the intent, the
write-ahead graph, the name claim; before and after the tip CAS; after the
finish, the commit; before a rename's release — and a writer crashed inside its
lease are the engine's suite, run over `mem` and over the same keyvalue stores
(`crates/holon-vcs/tests/kv.rs`): `verify` is clean afterwards, the op landed
exactly when its commit point had happened, a retry lands it exactly once.

Measured on an M2 Max (release build): the six scenarios run in parallel in
under 2 s wall time, F (record-store's 134 symbols) the longest at 1.6 s.

## Not yet

- **No authentication at the gateway.** Anyone who reaches `:3942` edits every
  workspace; the app is tailnet-only for that reason.
- **`materialize` writes on the host's machine**, under its `--dir`; an agent
  elsewhere reads the snapshot's blobs with `read-blob`. WASI has no chmod, so an
  executable entry is written `0644`.
- The engine's own open items (ADR-0099, *Still open*): the lease as a liveness
  knob, a tie that placement cannot order, methods as symbols, cross-file edges.
