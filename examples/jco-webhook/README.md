# Embed webhook-ingest in-process via jco

The **composition showcase** of this workspace. `webhook_ingest.wasm` here is
the COMPOSED artifact (`just compose-webhook`): the webhook-ingest component
plugged together with idempotency-guard via `wac`, so a single `.wasm` chains
two reusable capabilities — HMAC signature verification + replay dedup.

`jco transpile` turns that composed component into JS; this example calls its
exported `verifier` interface, and the imported `idempotency:guard/store` runs
in-process too (already satisfied by the compose — no separate wiring).

```
webhook_ingest.wasm   # the COMPOSED component (webhook-ingest + idempotency-guard)
src/
  keyvalue-shim.js     # host shim for wasi:keyvalue/store (Map) + __seed test hook
  config-shim.js       # host shim for wasi:config/runtime (idempotency default-ttl)
test/
  webhook.test.ts      # valid first delivery / replay / bad signature
gen/                   # produced by `jco transpile` (gitignored)
```

## Run

```bash
npm install
npm run transpile      # composed webhook_ingest.wasm -> gen/
npm test
```

The signing secret is seeded into the kv shim (`__seed`) and read by the
component at `secret-ref`; the test signs payloads with Node's `crypto` HMAC and
asserts: valid first delivery → `{accepted, !replay}`; same `delivery-id` →
`{!accepted, replay}` (the idempotency capability at work); bad signature →
`bad-signature` (rejected before any dedup).

> To refresh the composed wasm after rebuilding: `just compose-webhook` then copy
> `components/target/webhook_ingest.composed.wasm` here as `webhook_ingest.wasm`.
