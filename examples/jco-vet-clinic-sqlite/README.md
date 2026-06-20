# jco-vet-clinic-sqlite — the vet-clinic, with a durable SQLite KV store

The exact same app as [`jco-vet-clinic`](../jco-vet-clinic) — same components, same
frontend, same routes — but the key-value store is backed by **SQLite on disk**
instead of an in-memory Map, so **data survives restarts**.

## What changed (only the storage)

Nothing about the components or the app logic. `wasi:keyvalue@0.2.0-draft` is
synchronous, so this can't call an async DB inline; the shared shim
(`../jco-vet-clinic/src/shims/keyvalue.js`) keeps a synchronous in-memory
**mirror** for reads and **writes through** to a pluggable backend. This example
supplies that backend:

- `kv-backend.js` — a `better-sqlite3` store, one table `kv(bucket, key, value)`.
  `load()` hydrates the mirror on boot; `write`/`remove` persist each mutation.
  (better-sqlite3 is synchronous, so it's a natural fit.)

The base server picks it up via two env vars (set by this example's `start`):
`VET_KV_BACKEND=sqlite` + `VET_KV_BACKEND_MODULE=<abs path to kv-backend.js>`.

## Run

```bash
npm install
npm start            # transpiles the base components, boots on :3001 (PORT to override)
# create a pet, then Ctrl-C and `npm start` again — your data is still there.
```

DB file: `vet-clinic.sqlite` (override with `VET_SQLITE_PATH`).

## Test

```bash
npm test             # proves write-through + reload round-trips on disk
```

See the sibling persistence variants: [redis](../jco-vet-clinic-redis),
[nats](../jco-vet-clinic-nats), and the full wasmCloud + NATS-KV deployment
[vet-clinic-wasmcloud](../vet-clinic-wasmcloud).
