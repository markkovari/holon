# comp-jco-record-example

Exercises the `records:store@0.1.0` component in-process via
[`jco`](https://github.com/bytecodealliance/jco) — no host, no network.

`records:store` is a typed JSON **record store**: the layer the app domain sits
on instead of hand-rolling put / read / scan / id-generation. It gives you:

- **Named collections** of JSON-object records (`pets`, `orders`, …).
- **ULID ids** — lexicographically sortable, so `list-records` returns records
  in creation (time) order and pages cleanly with a cursor.
- **Secondary indexes** — declare `index-fields` on `create`; `find-by`
  resolves single-field matches in O(matches), and `query` intersects several
  indexed-field filters at once. Querying an un-indexed field matches nothing.
- **Optimistic revision locking** — every record carries a `revision`; `update`
  takes an `expected-revision` and throws `revision-conflict` (carrying the
  current revision) on a stale write. Pass `0` to skip the check.
- Input validation — non-object JSON is rejected with `invalid-json`.

The component imports `wasi:keyvalue/store` for persistence. Here it is backed
by a trivial in-memory `Map` (`src/keyvalue-shim.js`); swap that shim for
redis / sqlite / NATS and the component is unchanged.

## Run

```bash
npm install
npm test          # transpiles record_store.wasm -> gen/, then runs the tests
```

`npm run transpile` alone produces `gen/record_store.js`.
