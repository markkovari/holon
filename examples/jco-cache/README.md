# jco-cache — exercise cache:store in-process

Transpiles the `cache` component (package `cache:store`) with jco and runs it in
Node against shims:

- `src/keyvalue-shim.js` — in-memory `wasi:keyvalue` store (the cache's storage).
- `src/backing.js` — a fake backing store satisfying the cache's `source`/`sink`
  imports (the read/write-through + write-behind callbacks). Exposes test hooks
  `__seed` / `__backing`.

```bash
npm install
npm test     # 10/10: 6 primitives + 4 strategies
```

Covers get/set, miss, TTL expiry, no-expiry `ttl()`, delete/invalidate,
invalidate-prefix, and cache-aside / read-through / write-through / write-behind.
