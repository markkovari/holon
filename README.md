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

## Configuration

The contract is config-driven, with two layers:

**Runtime secrets/IdP wiring** (kv-seeded): OIDC issuer, client id/secret, HS256
secret — read from keyvalue (`oidc:issuer`, `oidc:client-id`, etc.).

**Deployment policy** (`wasi:config/runtime`): set per-deployment in the
`auth-guard` component `config` block in `infra/k8s/app.yaml`, read by the guest
at runtime — no rebuild needed. Every knob has an in-code default:

| Key | Default | Meaning |
|-----|---------|---------|
| `session-ttl` | `3600` | session lifetime (seconds) |
| `password-min-len` | `8` | min password length for local accounts |
| `jwks-cache-ttl` | `3600` | OIDC discovery + JWKS cache freshness (seconds) |
| `default-tenant` | `""` | tenant assumed when token/request carries none |

Verified live: changing `session-ttl`/`password-min-len` in the manifest and
re-applying (no rebuild) changes `expires_in` and password validation.

What is **deliberately static** (internal data-model, not policy): token prefixes
(`sess_`/`ref_`/`usr_`), the keyvalue link name (`default`), NATS key sanitization.
Changing these would break stored data, so they are not operator knobs.

## wasmCloud build recipe (hard-won — host 1.4.x / wasmtime 25)

Getting Rust components to actually *run* on the wasmCloud host (not just build)
required matching the host's exact WASI ABI. The working recipe:

1. **`wasi:http` pinned to `@0.2.0`** in `wit/auth.wit` + `wit/deps.toml`. The
   host (wasmtime 25.0.3) bridges `wrpc:http@0.1.0` ↔ `wasi:http@0.2.0`; building
   against 0.2.3 → `resource type mismatch` at invocation.
2. **Componentize with the wasmtime-25 reactor adapter**, not cargo-component's
   default: build the core module, then
   `wasm-tools component new <core>.wasm --adapt wasi_snapshot_preview1=wasi_snapshot_preview1.reactor.wasm`
   (adapter from `bytecodealliance/wasmtime` release **v25.0.3**). This makes
   `wasi:io`/`wasi:cli` coherent with what the host links.
3. **keyvalue link must be named `default`** (`name: default` on the link in
   `app.yaml`); the component opens the store with `store::open("default")` —
   wasmCloud routes wasi:keyvalue by link name. The JS bucket comes from the
   link's `bucket` config (`comp-auth`) + `enable_bucket_auto_create: 'true'`.
4. **NATS KV keys are sanitized** (`kv::safe`) — JetStream keys allow only
   `[-/_=A-Za-z0-9]`, so `:`/`@`/`.` in emails are `_XX` hex-escaped.
5. Push to an in-cluster OCI registry (`registry.wasmcloud.svc:5000`) via
   `wkg oci push --insecure`. **Bump the image tag** on every change — the host
   caches by tag.
6. Keep **one host replica**: the OAM operator can leave two ReplicaSets at 1,
   splitting the lattice so http-provider and component land on different pods
   and wrpc invocations fail. Scale the older RS to 0.

## Verified end-to-end (orbstack K8s, wasmCloud 1.4.1 host, v0.5.1 operator)
```
register   -> 201 + principal
login      -> 200 + access/refresh token
/me        -> 200 + principal
logout     -> 204
/me (after)-> 401 expired
sample-consumer no token        -> 401 invalid_token
sample-consumer token, no perm  -> 403 insufficient_scope
```

## Status / roadmap
- ✅ Contract WIT validated; all three components build to valid components.
- ✅ Local accounts (register/login/me/logout) via `accounts-app` over the contract.
- ✅ Deploy manifests for both wasmCloud 1.x (wadm) and 2.x (K8s operator CRDs).
- ⬜ Runtime deploy + end-to-end smoke (needs `wash` 1.x, or a 2.x cluster).
- ⬜ Pin the RC-provisional 2.x CRD fields (HTTP address, kv config) once the
  v2 operator docs stabilize.
- ⬜ IdP seed scripts (register OIDC client, mint demo tokens) for zitadel/ory.
- ⬜ wasip3 async revision once stable.
