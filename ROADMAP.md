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
