# Fastify + auth:identity (HTTP integration)

A Fastify (TypeScript) app guarded by the `auth:identity` contract. The app
talks to the deployed auth components **over HTTP** — no wasm in this Node
process — so the pattern works from any language/framework.

```
src/
  auth-client.ts   # typed client for the auth HTTP surface
  auth-plugin.ts   # Fastify plugin: requireAuth(target, action) + /auth/* routes
  server.ts        # example routes, public + guarded
```

## How it works

The auth stack (the `accounts-app` component) exposes:
- `POST /register`, `POST /login`, `GET /me`, `POST /logout`
- `POST /verify` `{target, action}` — **verify token + require permission**

`auth-plugin.ts` wraps these and decorates `requireAuth(target, action)`, a
Fastify `preHandler` that calls `/verify` with the request's bearer token. On
success it attaches the verified `principal` to `request.principal`; otherwise
it replies `401`/`403` straight from the auth service's status.

```ts
app.get("/orders",
  { preHandler: app.requireAuth("orders", "read") },
  async (req) => ({ viewer: req.principal!.subject }));
```

## Run

1. Deploy the auth stack and port-forward the accounts-app HTTP port to 8001
   (see `comp/README.md`):
   ```bash
   kubectl port-forward -n comp-auth pod/<host-pod> 8001:8001
   ```
2. Start the app:
   ```bash
   cd comp/examples/fastify-app
   npm install
   AUTH_BASE_URL=http://localhost:8001 npm run dev
   ```

## Verified flow

```bash
curl localhost:3000/public                                  # 200 (no auth)
curl localhost:3000/orders                                  # 401 (no token)

curl -XPOST localhost:3000/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"alice@shop.com","password":"hunter2hunter","tenant":"acme"}'   # 201

TOK=$(curl -s -XPOST localhost:3000/auth/login \
  -H 'content-type: application/json' \
  -d '{"email":"alice@shop.com","password":"hunter2hunter","tenant":"acme"}' \
  | sed -E 's/.*"access_token":"([^"]+)".*/\1/')

curl localhost:3000/auth/me     -H "Authorization: Bearer $TOK"   # 200 + principal
curl localhost:3000/orders      -H "Authorization: Bearer $TOK"   # 403 insufficient_scope
curl -XPOST localhost:3000/auth/logout -H "Authorization: Bearer $TOK"  # 204
curl localhost:3000/auth/me     -H "Authorization: Bearer $TOK"   # 401 (revoked)
```

`/orders` returns **403 by default** — RBAC is deny-by-default. Granting access
needs a role assignment (`rbac.assign-role` in the contract) and a role→permission
mapping; expose those via an admin route or seed them to wire up the 200 path.

## Tests

E2E suite (`test/e2e.test.ts`, `node:test` via `tsx`) drives the full guarded
flow through `app.inject` against the **real** auth backend:

```bash
AUTH_BASE_URL=http://localhost:8001 npm test
```

If the backend is unreachable the suite **skips** (not fails), so it's CI-safe
without the cluster. With the auth stack up it runs 8 real e2e assertions
(public/401/register/409/login/me/403/logout→401).

## Config

| Env | Default | Meaning |
|-----|---------|---------|
| `AUTH_BASE_URL` | `http://localhost:8001` | base URL of the accounts-app HTTP surface |
| `PORT` | `3000` | Fastify listen port |
