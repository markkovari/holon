# vcs — the agent-native code store, served

A code store for a swarm of agents editing one codebase at once, where git
fails them: there is no lock to wait on, two edits to two different functions
both land whatever order they arrive in, and two edits to the SAME function
become a conflict record with both versions kept — data a resolver agent reads
and settles — rather than an error or a half-finished merge. Every edit is an
automatic commit in an operation log; any op can be reverted; a crash at any
point of a write is recovered on the next read or at startup.
[ADR-0099](../adr/0099-an-agent-native-code-store.md) is the design; this is the
service that puts it on the network.

## How it fits together

```
agent ──HTTP──▶ comp-host ┌ vcs-gateway ──holon:vcs──▶ vcs-store ┐ ──HTTP──▶ comp-vcs ──▶ NATS JetStream (blobs, pointers, oplog)
                          └──────────── one composed component ──┘                   └─▶ SurrealDB (symbols, patches, conflicts)
```

| piece | what it does |
|---|---|
| `wit/vcs/vcs.wit` | the contract: `holon:vcs/code-store` (apply, resolve, revert, query, export, conflicts, oplog, verify, repair) and `holon:vcs/files` (ingest real files, materialize a snapshot, read a blob) |
| `crates/holon-vcs` | the engine — every decision: commutation, conflicts, names, positions, the write order and its recovery, symbol extraction. Pure; builds for wasm32-wasip2 and for the host |
| `comp-vcs` (`reconciler/src/bin/vcs.rs`) | the engine over its NATS + SurrealDB adapters, one HTTP route per WIT function, startup repair, a `--allow-path` for `materialize`. Native because a guest cannot hold a NATS or SurrealDB connection (ADR-0095) |
| `vcs-store` (`components/vcs-store`) | exports the contract; each call is one request to `comp-vcs` ([CONTRACT](../../components/vcs-store/CONTRACT.md)) |
| `vcs-gateway` (`components/vcs-gateway`) | imports the contract and serves it as `POST /v1/<function>` — the same JSON `comp-vcs` takes — for agents and tests |
| `apps/vcs.toml` | the app: gateway ⊕ store on `:3942`, `comp-vcs` on `127.0.0.1:8014` |

The JSON is defined once (`crates/holon-vcs/src/wire.rs`) and used by all three
processes' code; refusals are `{error, detail, message}` with the `vcs-error`
case, its payload, and a status by kind (409 for a race or a conflict, 404, 400,
503 for storage).

## Run it locally

```
docker compose -f infra/compose.yaml up -d nats                      # or any nats-server -js on :4222
docker compose -f infra/compose.yaml --profile graph up -d surreal   # SurrealDB 3 on :8000, in memory
cargo xtask compose vcs
cargo xtask host vcs                                                  # comp-host on :3942 + comp-vcs on :8014
```

`cargo xtask host` builds and starts `comp-vcs` from the `[[daemon]]` table
(`--nats-url`, `--surreal-url` and the allow-list come from `extra_args`/`allow`;
`VCS_NATS_URL`/`VCS_SURREAL_URL` override the defaults from the environment).
With no services at all, `comp-vcs --memory` keeps everything in memory.

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
RUNS=5 bash e2e/vcs.sh       # five times over the same services
bash e2e/vcs.sh c_crash      # a name filter, passed to the test binary
```

`e2e/vcs.sh` starts the compose SurrealDB (its own compose project, removed at
the end) — or uses `VCS_E2E_SURREAL_URL` if you have one — and a private
`nats-server -js` on a free port with a temp store (never the box's :4222),
builds the two components and `comp-host`, and runs
`reconciler/tests/e2e_vcs.rs`. Each scenario starts its own `comp-vcs` (own
buckets, own SurrealDB database, own port) and its own `comp-host` with the
composed gateway, and talks to nothing but the gateway:

| | scenario |
|---|---|
| A | two agents edit `compute_total` and `validate_order` — adjacent functions of one file — at once from one read point: both land, the second `commuted` naming the other's patch, and the export has both |
| B | two agents edit `compute_total` at once: one lands, one is `conflicted` with both versions verbatim; export and materialize are refused `unresolved-conflict`; a resolver merges; the materialized directory's `git write-tree` equals the snapshot's `git-tree` |
| C | `comp-vcs` aborts itself (`--crash-*-step`) at each step of the write order — after the blob, the intent, the write-ahead graph, the name claim; before and after the tip CAS; after the finish, the commit; before a rename's release — then restarts normally: `verify` is clean, the op landed exactly when its commit point had happened, startup repair reports it rolled forward or aborted, a retry lands it exactly once (`duplicate` if it had), one committed op carries the patch |
| D | `commuted` with `read-at` is exact (an edit read after is `applied`, one read before commutes past exactly it); without `read-at` it over-reports, as documented |
| E | eight renames to one name at once: one lands, seven `name-taken` naming it |
| F | `components/record-store`'s real files (`.rs`, `.wit`, `Cargo.toml`) ingested, exported and materialized byte for byte, git tree id checked against `git`; re-ingest writes nothing; then two agents ingest edited copies of `idlist.rs` at once — two neighbouring functions (both land), the same function (one conflict, resolved), two inserts at one spot (both land, no conflict) |
| G | a function moved by ingest is one `move`, and the export is exact; moved back, exact again |
| H | a writer crashed before its commit point holds `snapshot-export` (`concurrent-modification`) for the lease (`--lease-secs 4`), then the next reader fences it and export goes through; the edit never landed |

Without `VCS_E2E_NATS_URL` / `VCS_E2E_SURREAL_URL` every test prints `SKIPPED`
and returns. CI compiles the suite and does not run it (`--no-run`): a skip that
returns `ok` would be a green tick for a test that did not run.

Measured on an M2 Max (release build, SurrealDB 3.1.3 in memory in Docker, a
local nats-server 2.15): the eight scenarios run in parallel in 19–20 s wall
time, bounded by C (ten crash/restart cycles, 19–20 s); A, B, D, E 1.3–2.5 s,
G 1.7–2.9 s, F 3.8–4.9 s, H 5.4–6.6 s (four of them the lease). Five
consecutive runs over one set of services, all green.

## Not yet

- **No authentication at the gateway.** Anyone who reaches `:3942` edits every
  workspace; the app is tailnet-only for that reason. `comp-vcs` takes a
  `--token`, which the gateway's `vcs-store` sends.
- **`materialize` writes on the daemon's machine**, under its allow-list — which
  is the only filesystem there is (ADR-0023); an agent elsewhere reads the
  snapshot's blobs with `read-blob`.
- **SurrealDB's `:8000` is also `comp-fswatch`'s port** in `apps/fs-watcher.toml`;
  the two apps do not share a box as written.
- The engine's own open items (ADR-0099, *Still open*): the lease as a liveness
  knob, a tie that placement cannot order, methods as symbols, cross-file edges.
