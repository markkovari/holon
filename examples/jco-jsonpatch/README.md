# Embed json:patch in-process via jco

The `json:patch` component running **inside the Node process** — no wasmCloud, no
NATS, no host shims. It is pure compute: JSON in, JSON out. `jco transpile`
turns `jsonpatch.wasm` into JS; this example calls its exported `patcher`
interface directly.

Three operations, all over JSON strings:

- `applyPatch(document, patch)` — **RFC 6902** JSON Patch (`add`, `remove`,
  `replace`, `move`, `copy`, `test`). Throws a typed error tagged
  `invalid-json` | `path-not-found` | `test-failed` | `invalid-patch`.
- `applyMerge(document, mergePatch)` — **RFC 7386** JSON Merge Patch (`null`
  deletes a key; objects merge recursively).
- `diff(source, target)` — produces an RFC 7386 merge-patch such that
  `applyMerge(source, diff(source, target))` reproduces `target`.

Useful for HTTP `PATCH` endpoints and document/state sync where you want partial
updates with a precise, standards-defined semantics.

```
jsonpatch.wasm           # the built component (pure compute, standard WASI only)
test/
  jsonpatch.test.ts      # RFC 6902 ops, RFC 7386 merge, diff round-trip
gen/                     # transpile output (gitignored) -> gen/jsonpatch.js
```

## Run

```bash
npm install
npm run transpile        # jsonpatch.wasm -> gen/
npm test                 # behavioral checks
```

`jco transpile jsonpatch.wasm -o gen` — no `--map` flags, since the component
imports only standard WASI interfaces and computes in-process.
