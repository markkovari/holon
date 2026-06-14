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

## Storage (wasi:keyvalue) — TTL & migration

`wasi:keyvalue@0.2.0-draft` has **no native TTL/expiry**. The implementation
handles this in two ways:

- **Sessions** carry `expires-at` inside the stored value; `session.lookup`
  treats an elapsed entry as gone and deletes it lazily (no background sweep).
- **OIDC discovery / JWKS** caches store `"{expiry-epoch}:{json}"` and re-fetch
  when the prefix time has passed (`jwks-cache-ttl`).
- **Rate-limit** counters store `"{count}:{window-start}"`; an elapsed window
  starts fresh on next access.

Consequence: expired keys linger until next touched. With a NATS-backed bucket
you can additionally set a bucket-level TTL on the provider for hard GC.

**Migration:** values are versioned implicitly by their JSON shape. To evolve a
record, add `#[serde(default)]` fields (forward-compatible) or bump a `v` field
and branch on read. There is no schema registry; keep changes additive. Keys
are namespaced by prefix (`sess:`, `refresh:`, `user:`, `rbac:…`) so a migration
can scan one prefix at a time.

## Observability (audit log)

`auth-guard` emits one **JSON audit line per decision** to stderr (host-captured,
scrapable by an OTel/log collector). No secrets — only event, outcome, tenant,
subject, and a short detail:

```json
{"audit":true,"ts":1781440000,"event":"authorize","outcome":"deny","tenant":"acme","subject":"usr_…","detail":"orders:read"}
```

Each line carries an `id` (random per-event correlation handle) so the lines
emitted while serving one request can be grouped in a log/trace backend.
Events: `authorize` (allow/deny/error), `login`, `register`, `refresh_reuse`
(breach). Toggle with config `audit-enabled` (default on).

### Wiring to OpenTelemetry

The wasmCloud host emits OTel traces/metrics/logs natively; component stderr
(the audit lines) is captured into the host's log pipeline. Enable export on
the `WasmCloudHostConfig` (`infra/k8s/host.yaml`):

```yaml
spec:
  observability:
    enable: true
    endpoint: "http://otel-collector.observability.svc:4318"
```

Point an OTel collector at that endpoint; filter audit lines by `"audit":true`
and group by `id`. Full distributed-trace spans across components (propagating a
W3C `traceparent` through the wrpc calls) are a future enhancement — today the
correlation is per-component via the `id` field.

## Benchmarks

`bench/` measures the components two ways: **in-process** (jco, raw op cost) and
**HTTP roundtrip** (deployed on wasmCloud k8s). Headline: fast read paths are
~µs in-process vs ~ms over HTTP (~600× — the wrpc + provider + network cost, not
the component); argon2 dominates register/login (~26 ms) in both. See
`bench/README.md` + the `bench-*.png` charts.

## Reusable capabilities (their own WIT packages)

Beyond auth, the repo ships generic, composable capability components — each its
own package, each a worked example of WIT-first composition:

- **`ratelimit:guard`** (`components/rate-limiter`) — fixed-window failure
  counter; composed into auth-guard with `wac`.
- **`cache:store`** (`components/cache`) — TTL byte cache with all four caching
  strategies (Cache-Aside, Read-Through, Write-Through, Write-Behind). It
  *imports* a `source`/`sink` the consumer provides for the through/behind
  strategies. See its README + `examples/jco-cache` (10/10 e2e).

## Composition (auth-guard + rate-limiter)

Rate limiting lives in its **own** package/component, not inside auth — a second
worked example of WIT-first composition (a component importing another
component's interface):

- `ratelimit:guard@0.1.0` (`components/rate-limiter/wit/`) — a generic
  fixed-window failure counter (`check` / `record-failure` / `reset`). Reusable
  by any service, not auth-specific.
- `rate-limiter` component implements it (kv-backed, config-driven
  `max-attempts` / `lockout-window`).
- `auth-guard` **imports** `ratelimit:guard/limiter` and gates login.
- `just compose` runs `wac plug` to satisfy that import with the rate-limiter,
  producing one self-contained `auth_guard.composed.wasm`.

```bash
just compose   # build all + wac plug rate-limiter into auth-guard
```

The jco-embed example uses the composed artifact; its e2e proves a 6th failed
login returns 429.

## IdP & dev tokens

- **Local JWT, no IdP** — mint an HS256 token for testing the `jwt`/`authorizer`
  path (enable HS256 via `allowed-algs` and seed `hs256-secret` in kv):
  ```bash
  node infra/scripts/mint-hs256.mjs --secret <kv hs256-secret> \
    --sub u1 --tenant acme --iss https://local --aud comp-auth --scope "orders:read"
  ```
- **Real OIDC** — bring up an IdP and seed the `oidc:*` config:
  ```bash
  infra/scripts/seed-idp.sh zitadel   # or: ory
  ```
  It starts the compose profile, waits for the issuer, and prints the
  `nats kv put comp-auth oidc:*` commands (Ory auto-registers a client; Zitadel
  registration is a one-time console step the script spells out).

## Using it

See **[USAGE.md](USAGE.md)** — the consumer guide: the one `authorize` call,
how token claims map to a `principal`, permissions/RBAC, token formats, all
config keys, and the error→HTTP table. Per-symbol reference lives in the doc
comments in `wit/auth.wit`.

## Examples

Two ways to consume the contract from a TypeScript/Fastify app:

- **`examples/fastify-app/`** — HTTP integration. Fastify calls the deployed auth
  components over HTTP; `requireAuth(target, action)` preHandler guards routes via
  the `accounts-app` `/verify` endpoint. Realistic microservice pattern.
- **`examples/jco-embed/`** — in-process. `jco transpile` runs `auth_guard.wasm`
  inside Node; the app calls the component's exports directly and supplies the
  WASI host imports (keyvalue, config) as JS shims. No wasmCloud/NATS needed.

Both verified end-to-end (register/login/me/logout + RBAC deny).

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
