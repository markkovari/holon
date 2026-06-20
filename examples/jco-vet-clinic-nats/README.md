# jco-vet-clinic-nats — the vet-clinic, with a durable NATS JetStream KV store

The exact same app as [`jco-vet-clinic`](../jco-vet-clinic) — same components, same
frontend, same routes — but the key-value store is backed by a **NATS JetStream
KV bucket** instead of an in-memory Map, so **data survives restarts**.

This is the **same store the wasmCloud `keyvalue-nats` provider uses**, so this
jco-side variant is the closest mirror of the production storage path.

## What changed (only the storage)

Nothing about the components or the app logic. `wasi:keyvalue@0.2.0-draft` is
synchronous, but NATS is async — so we can't call NATS inline. The shared shim
(`../jco-vet-clinic/src/shims/keyvalue.js`) keeps a synchronous in-memory
**mirror** for reads and **writes through** to a pluggable backend
asynchronously on each mutation. This example supplies that backend:

- `kv-backend.js` — a NATS JetStream KV store. `load()` hydrates the mirror on
  boot; `write`/`remove` persist each mutation asynchronously.

The base server picks it up via two env vars (set by this example's `start`):
`VET_KV_BACKEND=nats` + `VET_KV_BACKEND_MODULE=<abs path to kv-backend.js>`.

### Entry-key scheme

Everything lives in one JetStream KV bucket (`VET_NATS_KV`, default
`vetclinic`). The app's `(bucket, key)` pair is encoded into the NATS KV entry
key as `${bucket}/${key}` — e.g. `default/pet_rex`. NATS KV keys allow
`A-Za-z0-9-_/=.`, and the app's keys (`pet_`, `appt_`, `sess_`, `al_`, ...) plus
the `default` app-bucket are all within that set. `load()` splits on the **first**
`/` to recover the app bucket and key. Values are stored as **raw bytes** —
NATS KV holds `Uint8Array` natively, so there is **no base64** for values.

## Run

```bash
# A NATS server with JetStream enabled must be reachable (default :4222):
#   docker run -d --name vet-nats -p 4222:4222 nats:2.10-alpine -js
npm install
npm start            # transpiles the base components, boots on :3003 (PORT to override)
# create a pet, then Ctrl-C and `npm start` again — your data is still there.
```

Connection: `VET_NATS_URL` (default `nats://127.0.0.1:4222` — the loopback IP
avoids a `localhost` DNS-resolution hang seen with the nats client on some hosts),
bucket: `VET_NATS_KV` (default `vetclinic`).

## Test

```bash
npm test             # proves write-through + reload round-trips through NATS KV
```

See the sibling persistence variants: [sqlite](../jco-vet-clinic-sqlite),
[redis](../jco-vet-clinic-redis), and the full wasmCloud + NATS-KV deployment
[vet-clinic-wasmcloud](../vet-clinic-wasmcloud).
