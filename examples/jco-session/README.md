# Embed session-store in-process via jco

The `session:store` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `session_store.wasm` into JS; this
example calls its exported `store` interface directly.

```
session_store.wasm        # the built component
src/
  keyvalue-shim.js         # host shim for wasi:keyvalue/store  (in-memory Map)
  config-shim.js           # host shim for wasi:config/runtime  (default-ttl)
test/
  session.test.ts          # create / get / verify-csrf / update / refresh / revoke
gen/                       # transpile output  (gitignored)
```

## Run

```bash
npm install
npm run transpile         # session_store.wasm -> gen/
npm test                  # behavioral checks
```

The component creates a session (data + TTL), hands back an id and a CSRF token,
and supports `get`, `update-data`, `refresh`, `verify-csrf`, and `revoke`.
Sessions are looked up by id and disappear once revoked or expired —
`get`/`verify-csrf` then surface `not-found`.

The two non-standard imports are mapped to local shims at transpile time:

```
jco transpile session_store.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

Swap the in-memory `Map` in `keyvalue-shim.js` for redis/sqlite/NATS to persist,
or change `default-ttl` in `config-shim.js` (overridable via `DEFAULT_TTL`); the
component neither knows nor cares.
