# ADR-0134 — The code store is a component

*`comp-vcs` held NATS JetStream and a SurrealDB socket because a component could not, and wrote
checkouts to a directory because a component could not. Every decision was already `holon-vcs`'s
engine, which builds for wasm. The engine moves into `vcs-store`, on the app's own keyvalue; the
host opens a directory to an app that names one; the daemon goes.*

**Status: accepted.** Revises ADR-0099's service shape (*The service*); the engine, the wire, the
write order and the recovery policy are unchanged.

## The problem

ADR-0099 made `comp-vcs` native for three reasons: a held NATS connection (blobs, pointers, oplog),
a SurrealDB WebSocket (the graph), and `materialize` writing a directory. Blobs, pointers and the
oplog were already on a `Kv` trait with compare-and-set, which `comp:store/cas` (ADR-0065) gives a
component on every keyvalue backend. The graph was the one store with no such seam.

## The decision

- **`KvGraph`**: the graph over the same `Kv`, a record per symbol, patch and conflict, each moved
  by compare-and-set, and index lists (by component, by name, by dependency, by conflict key) that
  only grow and are written before their record. An index may name more than matches, so every
  lookup reads the records and filters; the per-record rules are the ones `MemGraph` runs. One
  document per workspace was tried first and measured: with eight writers it lost 64 races in a
  row before 20 000 patches, so it is not this. `KvBlobs` puts blobs in the same bucket.
- **`vcs-store` runs the engine in-process.** Every call is `holon_vcs::serve::dispatch`, the code
  `comp-vcs` served, so `vcs-gateway`'s `/v1/<function>` wire is unchanged and `comp-agentd`'s
  `--vcs-url` points at the vcs app instead of the daemon.
- **The host's `--dir <path>`**, repeatable and refused beside `--lattice-nats`, preopens a directory
  to the app at the same path; a manifest names them as `dirs`. `materialize` writes there with
  `std::fs`, and nothing outside one exists for the guest, so the preopen is the allow-list
  `--allow-path` was. Self-hosting makes each one a `StateDirectory`, so they must be under
  `/var/lib/`: the one place a dynamic user can write.
- **A manifest's `mem_cap_mb`** sets the host's `--mem-cap-mb`: the engine now runs in the guest,
  and an `ingest-tree` of a whole component does not fit the default 64 MiB. `apps/vcs.toml` sets 256.
- **No startup repair.** The engine settles a pending op wherever it meets one (a tip read, a claim,
  the oplog walk), so a guest that traps half-way is rolled forward or aborted by the next call past
  the lease; `repair` remains a call anyone can make for a whole workspace.
- **`reconciler/daemons/vcs` is deleted**, with `comp-vcs`, and with it `holon-vcs`'s NATS and
  SurrealDB adapters, whose only user it was. `native` is now the S3 blob store alone (ADR-0122).

## Consequences

- Every engine scenario, crashes at every step of the write order included, runs twice in CI: over
  `mem` and over the keyvalue stores (`crates/holon-vcs/tests/kv.rs`). The end-to-end suite
  (`reconciler/tests/vcs_app.rs`, the old `e2e_vcs` ported) runs in CI too; it needed NATS and
  SurrealDB before, and so never did. The two scenarios that `abort()`ed the daemon at a write step
  are the engine suite's now.
- `crates/holon-vcs/tests/bench.rs` runs on `KvGraph`: at 5 000 symbols, 20 000 patches and 1 000
  conflicts a write is ~25 µs and does not grow; listing a workspace's open conflicts is the slow
  lookup, at 1.5 ms.
- WASI has no chmod, so `materialize` writes an executable entry `0644`; the snapshot and its git
  tree still say executable.
- `vcs-store` is 1.7 MB: it carries the engine and the Rust extractor (`syn`).
- One keyvalue holds the whole store, so its durability is the app's `kv`: `sqlite` on one box,
  `nats` with `--kv-replicas 3` on a cluster (ADR-0111).
