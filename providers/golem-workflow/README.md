# golem-workflow — a native wRPC→Golem durable-worker provider

The first thing in this repo that isn't a wasm component: a **native wasmCloud
capability provider** that lets a component call a durable [Golem](https://golem.cloud)
worker through the typed `durable:workflow/orchestrator` contract. See
[`../../docs/capabilities/GOLEM.md`](../../docs/capabilities/GOLEM.md) for the design.

## What's verified (and what isn't)

- ✅ **Contract + mapping** — `wit/durable-workflow.wit` + the `Value` mapping in
  `src/lib.rs`, unit-tested (6 tests) against the real `golem_wasm_rpc::Value`.
- ✅ **Provider compiles** — `src/main.rs`: a real provider (wit-bindgen-wrpc
  `Handler` + `serve_provider_exports`). The tricky bit was pinning
  `wit-bindgen-wrpc 0.10` so its `wrpc-transport` (0.28) matches
  `wasmcloud-provider-sdk 0.17.1` — otherwise `WrpcClient: Serve` fails to resolve.
- ✅ **Live Golem e2e** — the bridge call (`invoke_golem`) invoked against a
  **running Golem 1.5** with a real deployed agent, asserting durable state
  advances. Automated: `GOLEM_E2E=1 cargo test`.
- ◻︎ **wasmCloud front-half** — a component calling the provider over wRPC on a
  lattice is **not run here**: the installed `wash 2.3` is the new component-shell
  (no classic host/`par`/wadm to run a native provider). The provider compiles;
  running it live needs the classic wasmCloud host.

## Reproduce

**Unit tests (no infra):**
```bash
cargo test              # 6 mapping tests; the live one skips without GOLEM_E2E
```

**Live e2e against a real Golem** (`bash e2e.sh`, or by hand):
```bash
# 1. Golem 1.5 (single self-contained binary — Golem's own local-dev path).
#    Prebuilt for this arch; no build. (Docker alt below.)
curl -fsSL -o .bin/golem \
  https://github.com/golemcloud/golem/releases/download/v1.5.5/golem-$(uname -m)-apple-darwin
chmod +x .bin/golem
.bin/golem server run --clean &          # gateway :9006, worker svc :9007

# 2. deploy the bundled demo agent (a durable counter — stands in for a workflow)
cd golem-agent && ../.bin/golem build && ../.bin/golem deploy -Y && cd ..

# 3. run the provider's bridge against it
GOLEM_E2E=1 cargo test bridge_invokes_a_real_durable_golem_worker
```

### Binary vs docker

Both are here. The **verified** e2e above uses Golem's single `golem server run`
binary — one process, embedded sqlite, **zero auth**, Golem's own quickstart path.

Golem's docker path is vendored in [`golem-docker/`](golem-docker) (their
official 9-service `published-postgres` compose). It **stands up cleanly**, but
driving the e2e through it also needs the CLI authenticated against the
production registry-service, which hit `AUTH_UNAUTHORIZED: Token not found` here
(a CLI-v1.5.5 ↔ images-v1.5.0 skew — see `golem-docker/README.md`). That extra
auth/version ops is precisely why the dev binary is the default: one process, no
token dance. The provider itself is backend-agnostic — it only needs `GOLEM_URL`
+ `Host` — so a version-matched compose works the same once seeded.

## Config (provider link-time)

| key | default | meaning |
|---|---|---|
| `GOLEM_URL` | `http://127.0.0.1:9006` | Golem API-gateway base |
| `GOLEM_HOST` | — | `Host` header for gateway subdomain routing (e.g. `bookapp.localhost:9006`) |
| `GOLEM_PATH_TEMPLATE` | `/counters/{workflow-id}/increment` | agent endpoint; `{workflow-id}` is substituted |
