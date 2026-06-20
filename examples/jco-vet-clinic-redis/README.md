# jco-vet-clinic-redis — the vet-clinic, with a durable Redis KV store

The exact same app as [`jco-vet-clinic`](../jco-vet-clinic) — same components, same
frontend, same routes — but the key-value store is backed by **Redis** instead of
an in-memory Map, so **data survives restarts**.

## What changed (only the storage)

Nothing about the components or the app logic. `wasi:keyvalue@0.2.0-draft` is
synchronous, so this can't call an async store inline; the shared shim
(`../jco-vet-clinic/src/shims/keyvalue.js`) keeps a synchronous in-memory
**mirror** for reads and **writes through** to a pluggable backend. This example
supplies that backend:

- `kv-backend.js` — a `redis` (node-redis v4) store. Each value lives at Redis
  key `vet:{bucket}:{key}`, base64-encoded so arbitrary bytes round-trip exactly.
  `load()` SCANs `vet:*` to hydrate the mirror on boot; `write`/`remove` persist
  each mutation. node-redis pipelines commands, so the shim's fire-and-forget
  write-through is fine.

The base server picks it up via two env vars (set by this example's `start`):
`VET_KV_BACKEND=redis` + `VET_KV_BACKEND_MODULE=<abs path to kv-backend.js>`.

## Run

```bash
# Need a Redis on :6379 — e.g.
docker run -d --name vet-redis -p 6379:6379 redis:7-alpine

npm install
npm start            # transpiles the base components, boots on :3002 (PORT to override)
# create a pet, then Ctrl-C and `npm start` again — your data is still there.
```

Redis URL: `redis://localhost:6379` (override with `VET_REDIS_URL`).

## Test

```bash
npm test             # proves write-through + reload round-trips through Redis
```

The test skips gracefully if no Redis is reachable.

See the sibling persistence variants: [sqlite](../jco-vet-clinic-sqlite),
[nats](../jco-vet-clinic-nats), and the full wasmCloud + NATS-KV deployment
[vet-clinic-wasmcloud](../vet-clinic-wasmcloud).
