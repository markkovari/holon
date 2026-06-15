# Embed idempotency-guard in-process via jco

The `idempotency:guard` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `idempotency_guard.wasm` into JS; this
example calls its exported `store` interface directly.

```
idempotency_guard.wasm   # the built component (copy of components/target/.../idempotency_guard.wasm)
src/
  keyvalue-shim.js        # host shim for wasi:keyvalue/store  (in-memory Map)
  keyvalue-sqlite.js      # host shim for wasi:keyvalue/store  (SQLite compiled to wasm, sql.js — durable)
  config-shim.js          # host shim for wasi:config/runtime  (default-ttl)
test/
  idempotency.test.ts     # reserve / in-progress / replay / forget   (Map shim)
  sqlite.test.ts          # cross-process durability                  (SQLite-wasm shim)
  writer.mjs              # child-process writer used by sqlite.test.ts
gen/                      # Map-backed transpile output     (gitignored)
gen-sqlite/               # SQLite-backed transpile output   (gitignored)
```

## Run

```bash
npm install
npm run transpile         # idempotency_guard.wasm -> gen/
npm test                  # behavioral checks
```

The two non-standard imports are mapped to local shims at transpile time:

```
jco transpile idempotency_guard.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

Swap the in-memory `Map` in `keyvalue-shim.js` for redis/sqlite/NATS to persist;
the component neither knows nor cares.

## SQLite-wasm backend (durable)

`keyvalue-sqlite.js` satisfies the **same** `wasi:keyvalue/store` import with
SQLite **compiled to WebAssembly** (`sql.js`). The identical
`idempotency_guard.wasm` guest runs unchanged — only the host shim swaps. Two
independent wasm modules (the SQLite engine and the guest component) meet at the
keyvalue boundary, and records persist to a `.sqlite` file on disk.

```bash
npm run transpile:sqlite   # idempotency_guard.wasm -> gen-sqlite/ (SQLite shim)
npm run test:sqlite        # cross-process durability proof
```

`sqlite.test.ts` has a child process write + complete a key, exit, then a fresh
process replay it — demonstrating durability a `Map` cannot provide. Point the
store at a path with `KV_SQLITE_PATH` (default `./idem.sqlite`).

| | Map shim (`gen/`) | SQLite-wasm shim (`gen-sqlite/`) |
|---|---|---|
| Storage | in-process `Map` | SQLite-in-wasm, persisted to a file |
| Survives restart | no | yes |
| Deps | none | `sql.js` |
| Use for | fast hermetic tests, the bench | durable single-process / edge |
