# Embed config-store in-process via jco

The `config:store` component running **inside the Node process** — no wasmCloud,
no NATS. `jco transpile` turns `config_store.wasm` into JS; this example calls
its exported `store` interface directly.

`config:store` is **typed, versioned, WRITABLE** runtime configuration: each
value is a `variant { text, integer, boolean, decimal, json }` carrying its own
`version` and `updated` timestamp. `set-if` gives optimistic concurrency
(compare-and-swap on the version). It is the **writable sibling** of the
read-only `wasi:config` and of `feature-flags` — same in-process embedding, but
callers can mutate and the store tracks every revision.

```
config_store.wasm        # the built component
src/
  keyvalue-shim.js        # host shim for wasi:keyvalue/store  (in-memory Map)
test/
  config.test.ts          # typed values / versioning / set-if / keys / delete / isolation
gen/                      # transpile output  (gitignored)
```

## Run

```bash
npm install
npm run transpile         # config_store.wasm -> gen/
npm test                  # behavioral checks
```

The component imports `wasi:keyvalue` + `wasi:clocks`. jco auto-shims the
standard `clocks`; the non-standard `keyvalue` is mapped to a local shim at
transpile time:

```
jco transpile config_store.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js
```

Swap the in-memory `Map` in `keyvalue-shim.js` for redis/sqlite/NATS to persist;
the component neither knows nor cares.

## Value types (jco JS mapping)

The WIT `value` variant maps to `{ tag, val }` in JS:

| tag | WIT | JS `val` |
|---|---|---|
| `text` | `string` | `string` |
| `integer` | `s64` | `bigint` |
| `boolean` | `bool` | `boolean` |
| `decimal` | `f64` | `number` |
| `json` | `string` | `string` |

`get` throws `{ payload: { tag: 'not-found' } }` for an unset key; `set-if`
throws `{ payload: { tag: 'version-conflict', val: currentVersion } }` when the
expected version is stale.
