# comp — Evaluation & Roadmap

## What this is worth today

A **WIT-first** auth + RBAC contract (`auth:identity`) with a Rust reference
implementation that provably runs **two ways from the same `.wasm` bytes**:
on a wasmCloud Kubernetes cluster (NATS-backed `wasi:keyvalue`) and **in-process**
in Node via `jco` (in-memory shim). Both paths pass identical e2e tests. The
"build once, swap the host" promise of the Component Model is demonstrated, not
just claimed.

**Strong as:** a reference for WIT-first design, a worked wasmCloud-on-k8s deploy
(with a hard-won build recipe — see README), and a template for consuming a
component over HTTP *or* embedded.

**NOT yet production auth.** The contract design is sound; the *implementation*
has security gaps (hand-rolled crypto glue, incomplete JWT validation). Treat the
current state as a demo/learning artifact until Tier 1 lands.

## Known weaknesses (ranked)

### Security — block production use
1. **JWT validation incomplete** — `jwt::verify` checks signature + `exp` but not
   `iss`, `aud`, or `nbf`. Enables audience-confusion / cross-service token reuse.
2. **Algorithm-confusion surface** — HS256 (shared secret) and RS/ES256 (JWKS)
   are accepted by one verifier without pinning the expected alg per issuer.
3. ~~**Hand-rolled crypto glue** — manual HMAC-SHA256.~~ DONE: HMAC now uses the
   vetted RustCrypto `hmac` crate (constant-time `verify_slice`); RSA/EC verify
   already used vetted `rsa`/`p256`. JWKS/base64 parsing remains, covered by tests.
4. **Refresh not replay-safe** — rotation deletes the old token but stolen-token
   reuse isn't detected (should invalidate the whole session family).
5. **No rate limiting / lockout** on login/register — credential stuffing + user
   enumeration (the constant-time path in `verify_password` is not guaranteed).

### Correctness / robustness
6. **No Rust unit tests** — only the TS examples are tested; the crypto/session/
   rbac logic itself has zero coverage.
7. **JWKS cache** has no `kid`-rotation handling on miss (stale key → false reject).
8. **RBAC has no admin path** — `assign-role` is in the contract but unreachable
   over HTTP, so the *authorized* (200) path is never exercised, only deny (403).

### Ops
9. **2-ReplicaSet lattice split** — deploy can leave two hosts; currently fixed by
   a manual `kubectl scale rs … 0`. Should be automated.
10. **No observability** — no tracing, no audit log of auth decisions; KV has no
    TTL/migration story.

## Roadmap

### Tier 1 — make it trustworthy  (in progress)
- [ ] JWT: validate `iss` + `aud` + `nbf`; pin expected alg per issuer (config:
      `expected-issuer`, `expected-audience`).
- [ ] Rust unit tests for jwt / session / rbac / accounts.
- [ ] Refresh-token reuse detection (invalidate session family on replay).

### Tier 2 — make it usable  (done)
- [x] Admin/RBAC routes (`assign-role`, `set-role-permissions`) + an e2e proving
      the **200 authorized** path (403 before grant, 200 after). Session
      principals re-resolve roles each check, so grants take effect immediately.
- [x] Rate limiting + lockout on login as a **separate `ratelimit:guard`
      package + `rate-limiter` component**, composed into auth-guard with `wac`.
      A second worked example of WIT-first composition (component imports
      component). e2e: 6th failed login → 429.
- [x] Replace hand-rolled HMAC with the vetted RustCrypto `hmac` crate
      (constant-time verify). Added JWT-path e2e: alg-pinning rejection + malformed.

### Tier 4 — make it learnable  (done)
- [x] Exhaustive WIT doc comments: claim-mapping table, token formats, config
      keys, per-variant HTTP statuses, scope/role semantics, refresh-family model.
- [x] Implementation docs: `lib.rs` module map + storage-key layout + claim
      handling; `USAGE.md` integration guide for consumers.

### Tier 3 — make it shippable  (done)
- [x] `just deploy-k8s` + `just k8s-collapse` — applies host + OAM app and scales
      stale host ReplicaSets to 0, leaving one lattice host. No manual RS dance.
- [x] Structured audit log of auth decisions (JSON to stderr, OTel-scrapable):
      authorize/login/register/refresh_reuse, secret-free, `audit-enabled` toggle.
      (Full distributed-trace spans left as a future enhancement.)
- [x] KV TTL/migration documented: in-value expiry pattern (sessions, JWKS cache,
      rate-limit windows), lazy delete, additive-JSON migration. See README.

### Beyond — composable capabilities

- [x] **Trace propagation**: `authorizer.authorize-traced(token, perm, traceparent)`
      threads the caller's W3C trace context into audit events (real `trace_id` +
      child `span_id`), correlating an authz decision to the originating request
      across the component boundary. (`authorize` unchanged — non-breaking.)
- [x] **`cache:store`** — a generic TTL cache as its own package + component
      (third composable capability, alongside `ratelimit:guard`). Primitives
      (get/set/ttl/invalidate/invalidate-prefix) + **all four caching
      strategies**: Cache-Aside (consumer pattern), Read-Through (`get-through`
      via imported `source`), Write-Through (`put-through` via `sink`),
      Write-Behind (`put-behind` + `flush`). e2e: 10/10 in `examples/jco-cache`.

### Tier 5 — optional polish  (done)

- [x] **Evaluated `jwt-compact`** as a full JWT framework. It builds clean to
      `wasm32-wasip1` (RustCrypto backend, no ring/getrandom issues). **Decided
      NOT to swap:** it uses the same underlying crates we already do
      (`rsa`/`p256`/`hmac`/`sha2`), does not provide JWKS resolution (we'd keep
      that anyway), and our claim-validation/alg-pinning layer is already
      unit-tested. Swapping would rewrite working code for no security gain.
      Revisit only if we drop JWKS or want a JWE/nested-token feature it offers.
- [x] IdP seed scripts: `infra/scripts/mint-hs256.mjs` (local dev JWT, no IdP)
      + `infra/scripts/seed-idp.sh zitadel|ory` (bring up IdP, register client,
      print kv-seed commands). JWT happy-path now e2e-tested (valid HS256 → 200,
      wrong secret → 401).
- [x] OTel: per-event `id` correlation in audit lines + host OTel export wiring
      documented (README). Full cross-component trace spans remain future work.

## Status

- ✅ Contract + impl + infra; e2e on wasmCloud k8s and jco in-process.
- ✅ Config-driven policy via `wasi:config/runtime`.
- ✅ TS examples (HTTP + jco) with passing e2e suites.
- ✅ Tiers 1–4 complete. The auth itself is hardened; remaining work is optional
  polish (full OTel spans, more IdP seed scripts, a vetted full-JWT-framework swap).

## TODO — next up

### Native wRPC-to-Golem capability provider

- [ ] Build a native wasmCloud (v2.x) **capability provider** in Rust that
      bridges native wRPC calls on the NATS lattice to durable **Golem**
      workers: a component calls a typed WIT interface over wRPC, the provider
      maps the typed Rust structs to `golem_wasm_rpc::Value`, invokes the Golem
      worker via its HTTP client, and returns the typed result. First provider
      in this repo (everything so far is components + hosts). Full work brief:

<details>
<summary>Work brief (verbatim)</summary>

```
You are an expert systems-level Rust engineer specializing in the WebAssembly Component Model, wasmCloud (v2.x), and native wRPC (WebAssembly RPC) transports over NATS.

Your task is to write a complete, production-grade wasmCloud Capability Provider in Rust that acts as a native wRPC-to-Golem adapter.

Core Architecture
This provider runs as a native wasmCloud Capability Provider process. It implements the target WIT world using "wit-bindgen-wrpc" to automatically handle native, typed serialization and deserialization over NATS. Under the hood, it converts these typed Rust structs into Golem's universal "golem_wasm_rpc::Value" structure, triggers the durable Golem Worker via the Golem HTTP Client, and returns the strongly typed Rust result back to the caller over wRPC.

The control flow works as follows:

The wasmCloud Component makes a native wRPC call over the NATS Lattice.

The Golem wRPC Provider (this Rust application) intercepts the call.

The provider uses the "wit_bindgen_wrpc::generate!" macro to implement the generated asynchronous Rust Trait natively.

The provider directly maps typed Rust inputs to the Golem dynamic "Value" types.

The provider invokes Golem's REST API using the "golem_client::api::WorkerClient".

The provider maps Golem's returned values back to the strongly typed Rust return types.

Technical Specs and Dependencies
Generate a standard Rust binary crate project. Ensure your Cargo.toml targets these dependencies:

wit-bindgen-wrpc (For generating typed async Rust server bindings from WIT)

wasmcloud-provider-sdk (For handshake, linking, and managing the host-provider connection)

wrpc-transport-nats (For serving the generated wRPC bindings over the NATS bus)

golem-client (Golem's API Client SDK)

golem-wasm-rpc (with host feature enabled, for translating WIT types to Golem values)

tokio (multi-threaded async runtime)

tracing (structured logging)

The WIT Contract
Create a "wit/world.wit" file containing this exact contract:

Code snippet
package local:workflow;

interface orchestrator {
    record run-request {
        workflow-id: string,
        payload: string,
    }

    trigger-workflow: func(req: run-request) -> result<string, string>;
}

world golem-provider {
    export orchestrator;
}
Code Requirements
Please generate the following files:

Cargo.toml: Fully resolved dependencies using correct crate versions for a modern wasmCloud v2 ecosystem.

wit/world.wit: The interface definition provided above.

src/main.rs:

Invoke "wit_bindgen_wrpc::generate!({ world: "golem-provider" })" to build the traits.

Implement the async handler trait for the "orchestrator" interface. The signature must match the generated asynchronous signature, returning a standard "Result<Result<String, String>, ...>".

Instantiate Golem's "WorkerClient" during startup. Use environment variables (like GOLEM_URL and GOLEM_TEMPLATE_ID) passed during wasmCloud startup to configure the Golem endpoint.

Map the Rust types ("RunRequest" record) cleanly to "golem_wasm_rpc::Value" using helper mapping blocks (specifically matching a Record containing string fields).

In main(), initialize the wasmcloud-provider-sdk connection, obtain the NATS client, and pass the NATS transport directly to the generated serve function from the wRPC bindings to run the async server loop.

wadm.yaml: An application manifest showing how a frontend wasmCloud API component links to this provider to trigger durable workflows on Golem.

Ensure all Rust code is strictly typed, handles errors safely, and includes clean, descriptive error logging via the tracing crate.
```

</details>

### Helpdesk (HELPDESK.md)

- [ ] Rungs 2–7: multi-tenant + API keys + quotas, event-bus fan-out +
      notifications + signed webhooks, `mail-parse`, SLA timers + search,
      billing rollup, AI drafts. Rung 1 is done (`components/helpdesk-domain`,
      `examples/jco-helpdesk`, `just host-helpdesk` on the native host + NATS).

### Conduit / RealWorld (CONDUIT.md) — done

- [x] The full RealWorld ("Conduit") spec as one `conduit-domain` component +
      `auth-guard` + `record-store` + `slug`, run on the native Rust host.
      **Passes the official RealWorld Hurl conformance suite 100% (13/13 files,
      154 requests)** — `just conformance-conduit`; suite vendored + pinned under
      `examples/conduit/conformance`. Rust e2e (`just e2e-conduit`) + app-path
      bench (`bench/CONDUIT-BENCH.md`, round 13). This is the first showcase
      validated against an *external, objective* test suite rather than our own.
- [ ] Optional: password rotation + email rename in auth-guard (the two flagged
      conformance caveats), a `?search` extension (`search-index`), and a stock
      RealWorld frontend SPA served from `--static-dir`.

### Saga / durable orchestration (SAGA.md) — done

- [x] A durable **trip-booking saga** (`saga-domain`) — flight → hotel → car with
      **compensation** on failure — composed over `fsm-workflow` + `record-store`
      + `idempotency-guard` + `event-bus` + `sched:timer` + `id-generate`. The
      first showcase exercising **compensation + durable, resumable execution**:
      a flaky leg retries then gives up + compensates; `pump` advances one
      persisted step at a time; and the saga **survives a host kill and resumes**
      on NATS (`just durable-saga` → PASS). `just e2e-saga` (commit + all
      compensation/retry paths) + app-path bench (`bench/SAGA-BENCH.md`).
- [ ] Extract a generic `saga:orchestrator` contract (arbitrary step +
      compensation definitions), and land the Golem wRPC provider so a leg can be
      a real durable Golem worker (the roadmap item above).
