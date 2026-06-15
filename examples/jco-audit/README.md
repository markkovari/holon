# Embed audit-log in-process via jco

The `audit:log` component running **inside the Node process** — no wasmCloud, no
NATS. `jco transpile` turns `audit_log.wasm` into JS; this example calls its
exported `recorder` + `query` interfaces directly.

This component is the result of EXTRACTING auth-guard's inline stderr audit into
a standalone, durable, queryable capability. auth-guard now imports
`audit:log/recorder` (composed with `wac`) instead of logging inline; the
component still echoes each event to stderr, so the OTel/scrape path is
unchanged — the trail just also becomes durable + queryable.

```
audit_log.wasm        # the built component
src/
  keyvalue-shim.js     # host shim for wasi:keyvalue/store  (in-memory Map)
test/
  audit.test.ts        # record-event (id/ts stamping), recent (newest-first), by-trace
gen/                   # produced by `jco transpile` (gitignored)
```

## Run

```bash
npm install
npm run transpile      # audit_log.wasm -> gen/
npm test
```

`wasi:clocks` and `wasi:random` are auto-shimmed by jco's preview2-shim; only the
non-standard `wasi:keyvalue/store` is mapped to the local in-memory shim. Events
are stored at `al_{ts:020}_{id}`, so a key scan is chronological and "newest
first" is a reverse — see `src/lib.rs` in the component.
