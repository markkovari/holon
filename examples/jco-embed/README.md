# Embed auth-guard in-process via jco

The same `auth:identity` contract, but the **component runs inside the Node
process** — no wasmCloud, no NATS, no network hop. `jco transpile` turns
`auth_guard.wasm` into JS; this app calls its exported functions directly.

```
auth_guard.wasm        # the reference impl component (copy of the built artifact)
src/
  shims/keyvalue.js     # host shim for wasi:keyvalue/store  (in-memory Map)
  shims/config.js       # host shim for wasi:config/runtime  (policy via env)
  server.ts             # Fastify app calling the embedded component
gen/                    # produced by `jco transpile` (gitignored)
```

## How it differs from the HTTP example

| | `fastify-app` (HTTP) | `jco-embed` (in-process) |
|---|---|---|
| Auth runs | as wasmCloud components | inside this Node process |
| Transport | HTTP to `accounts-app` | direct JS function calls |
| You provide | nothing (services deployed) | **every WASI host import** |
| KV backend | NATS (via wasmCloud) | the `keyvalue.js` shim (swap for redis/sqlite) |
| Good for | microservices, polyglot | single-process apps, tests, edge |

## The host imports you must supply

jco auto-shims the standard WASI (`cli`, `clocks`, `io`, `random`, `http`,
`filesystem`) via `@bytecodealliance/preview2-shim`. The component also imports
two **non-standard** interfaces, mapped to local files at transpile time:

```
jco transpile auth_guard.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/shims/keyvalue.js \
  --map wasi:config/runtime@0.2.0-draft=../src/shims/config.js
```

- `keyvalue.js` exports `Bucket` + `open` (flat named exports — jco imports them
  destructured). Here it's an in-memory `Map`; point it at real storage to persist.
- `config.js` exports `get`/`getAll` — the policy knobs (session-ttl,
  password-min-len, …), here from env.

## Run

```bash
cd comp/examples/jco-embed
npm install
npm start            # runs `jco transpile` then the server (PORT=3001)
```

## Verified flow

```bash
B=http://localhost:3001
curl -XPOST $B/auth/register -H 'content-type: application/json' \
  -d '{"email":"a@b.com","password":"hunter2hunter","tenant":"acme"}'      # 201
TOK=$(curl -s -XPOST $B/auth/login -H 'content-type: application/json' \
  -d '{"email":"a@b.com","password":"hunter2hunter","tenant":"acme"}' \
  | sed -E 's/.*"accessToken":"([^"]+)".*/\1/')
curl $B/auth/me -H "Authorization: Bearer $TOK"     # 200 + principal
curl $B/orders  -H "Authorization: Bearer $TOK"     # 403 insufficient_scope
curl -XPOST $B/auth/logout -H "Authorization: Bearer $TOK"   # 204
curl $B/auth/me -H "Authorization: Bearer $TOK"     # 401 expired
```

## Tests

Fully self-contained E2E suite (`test/e2e.test.ts`) — the component runs
in-process with the in-memory kv shim, so **no cluster / NATS / network**:

```bash
npm test    # runs `jco transpile` then 9 e2e assertions
```

Covers register/409/login/401-bad-creds/me/403-RBAC/401-no-token/logout→401 plus
the config-driven password-min-len (from the config shim). Ideal for CI.

## Gotchas (learned building this)

- WIT `u64` (expires-at / expires-in) arrives as JS **BigInt**; `JSON.stringify`
  throws on it. `server.ts` sets a reply serializer that coerces BigInt → Number.
- A WIT `result` error is **thrown**, not returned. The thrown `auth-error`
  variant is `{ tag, val? }` (sometimes under `.payload`); map `tag` → HTTP status.
- jco `--map` paths resolve **relative to the output dir** (`gen/`), so the shims
  are referenced as `../src/shims/...`.
- The component is the unmodified `auth_guard.wasm` built for wasmCloud — same
  bytes run here and on the cluster. Only the host imports differ.
