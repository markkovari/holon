# jco-websign

Exercises the `webhook:sign` component in-process via [jco](https://github.com/bytecodealliance/jco).

This is the **send** side of outbound webhooks: it produces Stripe-style
(`t=<ts>,v1=<hex>`) and GitHub-style (`sha256=<hex>`) HMAC-SHA256 signatures so
your service can sign payloads before delivering them. It is the mirror image of
the verify performed by `webhook:ingest` on the receiving end — the same secret
and scheme on both sides round-trip cleanly.

- The signing secret is supplied by the caller (no embedded config).
- The HMAC clock (`wasi:clocks`) is **auto-shimmed by jco** — no manual shim.

## Interface

`signer` from package `webhook:sign`:

- `sign(body, secret, scheme)` → `{ header, timestamp }` (signs at now)
- `signAt(body, secret, scheme, timestamp)` → `{ header, timestamp }`
- `verify(body, header, secret, scheme, toleranceSeconds)` → throws
  `malformed-signature` | `signature-mismatch` | `timestamp-out-of-tolerance`

`scheme` is `'stripe' | 'github'`; `body` is a `Uint8Array`; timestamps are
`bigint` (u64). A `toleranceSeconds` of `0` skips the time-window check.

## Run

```bash
npm install
npm test
```

`npm test` transpiles `webhook_sign.wasm` into `gen/` and runs the test suite.
The known-answer test asserts the component's HMAC-SHA256 matches `node:crypto`
byte-for-byte.
