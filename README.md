# comp — WIT-first Universal Auth + RBAC

A **WIT-first** WebAssembly workspace defining a **universal authentication +
RBAC contract** (`auth:identity`) that any component can consume. The WIT is the
product; the Rust components and infra exist to prove the contract.

```
comp/
  wit/
    auth.wit            # THE contract: auth:identity@0.1.0 (+ vendored deps/)
    deps/  wkg.lock     # pinned WASI deps, version-controlled
  components/           # cargo workspace (cargo-component, wasm32-wasip2)
    auth-guard/         # reference impl — exports the full auth surface
    sample-consumer/    # HTTP app — guards its endpoint with one authorize() call
    accounts-app/       # HTTP register/login frontend — calls accounts+session+authorizer
  infra/
    compose.yaml        # NATS (always) + Zitadel | Ory (profiles)
    wadm.yaml           # wasmCloud app: components + providers + links
    .env.example
  Justfile
```

## The contract (`wit/auth.wit`)

Package `auth:identity@0.1.0`. Interfaces:

| Interface    | Role |
|--------------|------|
| `types`      | shared records: `principal`, `permission`, `claims`, `token-pair`, `auth-error` |
| `authorizer` | **the consumer API** — `authorize(token, required) -> result<principal, auth-error>` |
| `jwt`        | stateless JWT verify (RS256/ES256/HS256) |
| `oidc`       | IdP-agnostic discovery + JWKS + code exchange |
| `session`    | stateful sessions: `issue` / `refresh` / `revoke` / `lookup` |
| `accounts`   | local users: `register` / `login` / `verify-password` / `change-password` (argon2) |
| `rbac`       | roles → permissions, per tenant |

Worlds:
- **`consumer`** — what an app imports: just `authorizer`. The contract surface.
- **`consumer-http`** — `consumer` + `wasi:http/incoming-handler` (for HTTP apps).
- **`authority`** — the *implementation* world (exports everything, imports the
  host capabilities it needs). The **only** place backend capabilities appear.

### Backend- and IdP-agnostic by design
The contract names **no** storage backend and **no** vendor IdP:
- Sessions/roles are "held by the implementation" — the `authority` world imports
  a *generic* `wasi:keyvalue` capability, bound at deploy time to any provider
  (here, a NATS-backed one — chosen in `infra/wadm.yaml`, never in WIT).
- OIDC is standard discovery + JWKS only; Zitadel or Ory plug in via issuer URL.

## Build & verify

```bash
just vendor      # fetch + vendor WASI WIT deps (already committed)
just wit-check   # validate the contract resolves
just build       # cargo component build --release (both components)
just validate    # wasm-tools validate both .wasm components
just inspect     # show each component's imports/exports
just check       # wit-check + build + validate in one shot
```

Build output is a WebAssembly **component** (`wasm32-wasip1` core module +
adapter → wasip2 component). `wasm32-wasip3` is RC-only as of mid-2026; the WIT
is shaped to survive a future async refactor but does not depend on p3.

## Run the stack

```bash
# infra — NATS + one IdP profile (needed for both deploy models)
just up-zitadel       # OIDC issuer at http://localhost:8080
#   or
just up-ory           # OIDC issuer at http://localhost:4444
```

The components are plain WASI p2 components — runtime-independent. Only the
**deploy manifest** is wasmCloud-version-specific. Two options:

### wasmCloud 1.x — wadm / OAM (`infra/wadm.yaml`)
The classic standalone flow. Needs `wash` (1.x).
```bash
wash up                       # separate shell
just deploy                   # wash app put + deploy infra/wadm.yaml
# sample-consumer -> :8000, accounts-app -> :8001
```

### wasmCloud 2.x — Kubernetes operator (`infra/k8s/`)
2.0 moved orchestration into a **Kubernetes operator**: workloads are CRDs
(`WorkloadDeployment` + `WasmCloudHostConfig`), not a standalone wadm process.
Components co-located in one `WorkloadDeployment` are wired by the operator;
host capabilities come from `hostInterfaces`.
```bash
# 1. push components to an OCI registry (replace REPLACE_ME in workload.yaml)
wash push ghcr.io/<you>/auth-guard:0.1.0      components/target/wasm32-wasip1/release/auth_guard.wasm
wash push ghcr.io/<you>/sample-consumer:0.1.0 components/target/wasm32-wasip1/release/sample_consumer.wasm
wash push ghcr.io/<you>/accounts-app:0.1.0    components/target/wasm32-wasip1/release/accounts_app.wasm

# 2. install the operator + apply CRDs
helm install wasmcloud-operator oci://ghcr.io/wasmcloud/charts/wasmcloud-operator
kubectl apply -f infra/k8s/host.yaml
kubectl apply -f infra/k8s/workload.yaml
```
> **Note:** some `infra/k8s/workload.yaml` fields (HTTP listen address binding,
> keyvalue NATS config placement) are marked **RC-provisional** — the v2 RC
> docs don't pin them yet. Confirm against your installed operator's CRD. The
> 1.x `wadm.yaml` path is the known-working reference.

### End-to-end via the register/login app (accounts-app on :8001)
```bash
# 1. register a user
curl -i -XPOST localhost:8001/register \
  -d '{"email":"a@b.com","password":"hunter2hunter","tenant":"acme"}'        # 201

# 2. log in -> get a session access_token
TOK=$(curl -s -XPOST localhost:8001/login \
  -d '{"email":"a@b.com","password":"hunter2hunter","tenant":"acme"}' \
  | sed -E 's/.*"access_token":"([^"]+)".*/\1/')

# 3. who am I (guarded by authorizer.introspect)
curl -i -H "Authorization: Bearer $TOK" localhost:8001/me                     # 200 + principal

# 4. log out (session.revoke), then /me is 401
curl -i -XPOST -H "Authorization: Bearer $TOK" localhost:8001/logout          # 204
curl -i -H "Authorization: Bearer $TOK" localhost:8001/me                     # 401
```

### Contract smoke test (the guarded sample-consumer on :8000)
```bash
curl -i localhost:8000/                                  # 401 (no token)
curl -i -H "Authorization: Bearer $TOK" localhost:8000/  # 403 (no demo:read perm) / 200 if granted
```

## Toolchain
`wasm-tools`, `wkg`, `cargo-component`, `docker compose`. Deploy adds: `wash`
(1.x path) **or** `kubectl` + the wasmCloud operator (2.x path). `wac` is not
required — components are linked at runtime, not statically pre-composed.

## Status / roadmap
- ✅ Contract WIT validated; all three components build to valid components.
- ✅ Local accounts (register/login/me/logout) via `accounts-app` over the contract.
- ✅ Deploy manifests for both wasmCloud 1.x (wadm) and 2.x (K8s operator CRDs).
- ⬜ Runtime deploy + end-to-end smoke (needs `wash` 1.x, or a 2.x cluster).
- ⬜ Pin the RC-provisional 2.x CRD fields (HTTP address, kv config) once the
  v2 operator docs stabilize.
- ⬜ IdP seed scripts (register OIDC client, mint demo tokens) for zitadel/ory.
- ⬜ wasip3 async revision once stable.
