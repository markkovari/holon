# Embed upload-policy in-process via jco

The `upload:policy` component running **inside the Node process** — no wasmCloud,
no NATS. `jco transpile` turns `upload_policy.wasm` into JS; this example calls
its exported `gate` interface directly.

The component does two jobs:

- **Validation** (`check`) — rejects disallowed content types (`type-not-allowed`)
  and over-sized uploads (`too-large`) against the configured policy.
- **Signed presigned tickets** (`authorize` / `redeem`) — issues an HMAC-signed
  ticket granting a one-time upload to a tenant-scoped object key, then verifies
  it on redeem (`invalid-ticket` for garbage or tampered tokens). This pairs with
  `blob:store`: the policy mints the ticket, the blob store honors it.

```
upload_policy.wasm        # the built component
src/
  config-shim.js          # host shim for wasi:config/runtime  (allowed-types, max-size, ticket-ttl, ticket-secret)
test/
  upload.test.ts          # check / authorize / redeem behavior
gen/                      # transpile output (gitignored)
```

## Run

```bash
npm install
npm run transpile         # upload_policy.wasm -> gen/
npm test                  # behavioral checks
```

The component also imports `wasi:clocks` (ticket expiry) and `wasi:random`
(token nonce). jco **auto-shims** both, so only `wasi:config/runtime` needs a
local shim, mapped at transpile time:

```
jco transpile upload_policy.wasm -o gen \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

`config-shim.js` is swappable — point it at real config (env, a secrets store,
the OAM `config:` block on wasmCloud) without touching the component. Override
the defaults via `ALLOWED_TYPES`, `MAX_SIZE`, `TICKET_TTL`, `TICKET_SECRET`.

Sizes and expiry cross the boundary as `u64`, surfacing in JS as `bigint` —
use `n` literals (`2048n`).
