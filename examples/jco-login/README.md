# Embed the composed login-app in-process via jco

The **multi-capability composition** demo. `login-app` is a consumer component
that imports three capability interfaces — `session:store`, `config:store`, and
`secrets:vault`. `wac plug` (run via `just compose-login` from repo root `comp/`)
satisfies all three by plugging in the matching capability components, producing
a single `login_app.composed.wasm` that **exports only** `login:app/auth@0.1.0`
and **imports only generic WASI** (`wasi:keyvalue/store`, `wasi:config/runtime`,
plus clocks/random/filesystem that jco auto-shims).

So from the host's point of view the whole login stack is one component: `jco
transpile` turns it into JS and this example calls its exported `auth` interface
directly — no wasmCloud, no NATS.

```
login_app.composed.wasm   # wac-composed: login-app + session:store + config:store + secrets:vault
src/
  keyvalue-shim.js        # host shim for wasi:keyvalue/store  (in-memory Map; shared by all three sub-caps, keys are prefixed)
  config-shim.js          # host shim for wasi:config/runtime  (master-key, test-only)
test/
  login.test.ts           # login / whoami / logout through the full composition
gen/                      # transpile output  (gitignored)
```

## Run

```bash
npm install
npm run transpile         # login_app.composed.wasm -> gen/
npm test                  # behavioral checks
```

The two non-standard imports are mapped to local shims at transpile time:

```
jco transpile login_app.composed.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

All three sub-capabilities share the **one** in-memory keyvalue store; their keys
are namespaced (`sess_` / `cfg_` / `sv_`) so they don't collide. Swap the `Map`
in `keyvalue-shim.js` for redis/sqlite/NATS to persist — the component neither
knows nor cares.

## What the test proves

A single `auth.login("alice","pw")` fans out across the composition:

- **session:store** mints the token + csrf and persists the session,
- **config:store** supplies the session-ttl that sets `expires`,
- **secrets:vault** fetches the pepper (envelope-encrypted with `master-key`).

`whoami` / `logout` then read and revoke that session. If any of the three plugged
components failed to wire up, these calls would throw — green tests mean the full
`wac plug` composition executes end-to-end in-process.

## Keys & secrets

`config-shim.js` provides `master-key` = base64 of 32 zero bytes — a **throwaway
test key**. Real deployments inject a real key via `wasi:config` (or
`MASTER_KEY`); never ship the hardcoded fallback.

## Regenerating the wasm

`login_app.composed.wasm` is checked in. Rebuild it with `just compose-login`
from repo root `comp/`, which builds `login-app` and the three capability
components and runs `wac plug` to compose them.
