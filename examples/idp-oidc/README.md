# External IdP (Ory Hydra / Zitadel) — verify real tokens in-process

The recommended production shape: a **mature IdP owns credentials + issues
tokens**; this project's `auth-guard` does the fast **per-request verification**.
This example proves that with *real* IdP-issued JWTs, verified in-process via jco
against the IdP's **live JWKS** — no mock keys.

```
src/verify.ts          mint a real Hydra JWT + introspect it via auth-guard
src/shims/keyvalue.js  in-memory wasi:keyvalue (JWKS cache)
src/shims/config.js    expected-issuer / allowed-algs / clock-skew (env-driven)
test/hydra.test.ts     real Hydra RS256 JWT -> verified; tampered -> rejected
test/zitadel.test.ts   Zitadel OIDC discovery + JWKS reachability
```

`wasi:http` is backed by jco's preview2-shim (real fetch), so the embedded
component genuinely fetches `{issuer}/.well-known/openid-configuration` +
`jwks_uri` and verifies the RS256 signature itself.

## Run

```bash
# Ory Hydra must issue JWT access tokens (compose sets STRATEGIES_ACCESS_TOKEN=jwt)
docker compose -f ../../infra/compose.yaml --profile ory up -d
docker compose -f ../../infra/compose.yaml --profile zitadel up -d   # optional

npm install
EXPECTED_ISSUER=http://localhost:4444 npm run verify   # mint + verify a real Hydra JWT
npm test                                               # 6 tests (skip if IdP down)
```

Example `verify` output:
```
token iss=http://localhost:4444 sub=… alg(header)=RS256
VERIFIED principal: {"subject":"…","tenant":"","roles":[],"scopes":["openid"],"expiresAt":…}
```

## How each IdP issues a token

- **Ory Hydra** — `client_credentials` grant yields a JWT directly (the example
  registers an ephemeral client + mints one). Hydra must run with
  `STRATEGIES_ACCESS_TOKEN=jwt` (opaque otherwise).
- **Zitadel** — tokens come from the OIDC code flow (browser) or a **machine
  user** with a JWT-profile key (`urn:ietf:params:oauth:grant-type:jwt-bearer`).
  That setup is admin-authenticated + multi-step; do it once in the Zitadel
  console, then pass the token via `ZITADEL_TOKEN` and the test verifies it for
  real. The verifier is issuer-agnostic — it follows the token's `iss`, so the
  path is identical to Hydra's (which is fully exercised).

## Why this matters

The verify path (`authorizer.introspect`/`authorize`) is the microsecond-cheap
per-request hot path (see `bench/`). Pairing it with a battle-tested IdP for
credential storage + token issuance is the recommended deployment: the IdP does
identity, this does fast embedded authz. Same `auth_guard.wasm` either way.
