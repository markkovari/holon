# Embed paginate-cursor in-process via jco

The `paginate:cursor` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `pagination.wasm` into JS; this example
calls its exported `cursors` interface directly.

```
pagination.wasm        # the built component
src/
  config-shim.js       # host shim for wasi:config/runtime (cursor-secret, max-page-size)
test/
  pagination.test.ts   # round-trip / tamper / clamp / page assembly
gen/                   # transpile output (gitignored)
```

## Keyset (cursor) pagination

Instead of `OFFSET`, the page boundary is encoded as a **keyset position** —
the `sort-key` + `last-id` of the edge row plus a direction. `buildPage`
assembles `next`/`prev` cursors from the first/last rows of a result window so
the caller can walk forward and backward without skipping or repeating rows.

## Tamper-evident opaque cursor (HMAC)

The cursor handed to clients is opaque: the position is serialized and signed
with an **HMAC** keyed by `cursor-secret`. `decode` re-verifies the signature, so
a client cannot forge or mutate a cursor to read rows it was never paged to —
any edit fails the MAC check and throws `invalid-cursor`. `clampLimit` bounds the
page size to `max-page-size` (and rejects 0 with `bad-limit`).

## Run

```bash
npm install
npm run transpile        # pagination.wasm -> gen/
npm test                 # behavioral checks
```

The non-standard `wasi:config/runtime` import is mapped to a local shim at
transpile time:

```
jco transpile pagination.wasm -o gen \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

`config-shim.js` is swappable: it supplies the same knobs (`cursor-secret`,
`max-page-size`) the OAM `config:` block would on wasmCloud. Point it at a real
secret store in production; the component neither knows nor cares.

WIT `option<position>` arguments map to plain JS — pass `undefined` to omit
(e.g. `buildPage(undefined, last, false, true)` for a forward-only page).
