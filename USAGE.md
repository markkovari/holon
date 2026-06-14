# Using auth:identity

How to consume the contract from your own component or app. The authoritative
per-symbol reference is the doc comments in `wit/auth.wit`; this is the prose
walkthrough.

## 1. The one call you need

Most consumers import a single interface, `authorizer`, and make one call per
request:

```wit
world my-app {
  import auth:identity/authorizer;
  export wasi:http/incoming-handler@0.2.0;  // or whatever your app does
}
```

```rust
use bindings::auth::identity::authorizer::{authorize, Permission};

let principal = authorize(
    bearer_token,                                  // raw token string
    &Permission { target: "orders".into(), action: "read".into() },
)?;
// Ok(principal)             -> allowed; use principal.subject / .tenant / .roles
// Err(InsufficientScope(_)) -> 403
// Err(Expired|InvalidToken) -> 401
// Err(Malformed(_))         -> 400
```

`authorize` verifies the token (JWT / OIDC / session — detected automatically),
builds the `principal`, and enforces the permission, in one shot. Use
`authorize_any(token, perms)` for "any of", or `introspect(token)` to verify
without a permission check.

## 2. What a `principal` contains, and where it comes from

A verified token's claims map onto the `principal` like this:

| principal field | from |
|---|---|
| `subject`   | JWT `sub` / session owner / local account id (`usr_…`) |
| `tenant`    | claim `tenant`, else `org`, else `urn:zitadel:iam:org:id`, else config `default-tenant` |
| `scopes`    | OAuth `scope` (space-delimited) or `scp` (array) |
| `roles`     | the **RBAC store**, by (tenant, subject) — *not* the token |
| `expires-at`| JWT `exp` |

Custom/private claims (e.g. `email`, `groups`) aren't on `principal`; read them
from `claims.raw` via `jwt.verify` / `oidc.verify-id-token` if you need them.

Roles are resolved server-side on purpose: **a token cannot grant itself
roles.** Authorization decisions come from scopes-in-token *or* roles-in-store.

## 3. Permissions & RBAC

A `permission` is `{ target, action }`, e.g. `{ "orders", "read" }`. `"*"` is a
wildcard in either field. A principal is allowed when:

- it holds a **scope** equal to `"target:action"` or `"*"`, **or**
- one of its **roles** maps (in the store, for its tenant) to a matching
  permission.

Grant roles + role→permission mappings via the `rbac` interface
(`assign-role`, and seed `permissions-of`). Until a subject has a role (or a
scope), every `authorize` returns `insufficient-scope` (403) — deny by default.

## 4. Token formats (reference impl)

| prefix / shape | meaning |
|---|---|
| `sess_<hex>` | opaque session access token → stateful lookup |
| `ref_<hex>`  | refresh token (rotated each `session.refresh`) |
| `usr_<hex>`  | generated subject id for a local account |
| `a.b.c`      | a JWS (JWT) → stateless signature + claim verification |

Sessions belong to a **family**; refreshing rotates the token. Reusing a
rotated refresh token is treated as theft and revokes the whole family.

## 5. Configuration

Policy is read at runtime via `wasi:config/runtime` (set per-deployment, no
rebuild). Defaults make it run with zero config:

| key | default | meaning |
|---|---|---|
| `session-ttl` | `3600` | session lifetime (s) |
| `password-min-len` | `8` | local-account password floor |
| `jwks-cache-ttl` | `3600` | OIDC discovery/JWKS cache (s) |
| `default-tenant` | `""` | tenant when none in token/request |
| `expected-issuer` | `""` | required JWT `iss` (`""` disables — **set in prod**) |
| `expected-audience` | `""` | required JWT `aud` (`""` disables — **set in prod**) |
| `allowed-algs` | `RS256,ES256` | JWS alg allow-list (anti-confusion; add `HS256` for dev) |
| `clock-skew` | `60` | `exp`/`nbf` tolerance (s) |

Secrets & IdP wiring are supplied through the keyvalue store, not config:
`oidc:issuer`, `oidc:client-id`, `oidc:client-secret`, `hs256-secret`.

Crypto note: signatures use vetted RustCrypto crates — `rsa` (RS256), `p256`
(ES256), `hmac`+`sha2` (HS256, constant-time verify), `argon2` (passwords). No
hand-rolled primitives, no `ring`/native deps (so it builds clean for wasip2).

> **Production checklist:** set `expected-issuer` + `expected-audience`, and keep
> `allowed-algs` to the asymmetric algs you actually use. Leaving issuer/audience
> empty disables those checks (fine for local/dev, unsafe in prod).

## 6. Two ways to integrate (worked examples)

- **Over HTTP** (`examples/fastify-app/`) — call the deployed components'
  endpoints; a `requireAuth(target, action)` preHandler hits `POST /verify`.
- **In-process** (`examples/jco-embed/`) — `jco transpile` the component into
  your Node process and call its exports directly, supplying the WASI host
  imports (keyvalue, config) as shims.

Same `auth_guard.wasm` runs both ways — only the host imports differ.

## 7. Error → HTTP mapping

Every consumer should map `auth-error` the same way (statuses are documented on
the variant in the WIT):

| variant | HTTP |
|---|---|
| `invalid-token` / `expired` / `invalid-credentials` | 401 |
| `insufficient-scope` / `unknown-tenant` | 403 |
| `already-exists` | 409 |
| `malformed` | 400 |
| `backend-unavailable` | 503 |
| `internal` | 500 |
