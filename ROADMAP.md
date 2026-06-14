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

### Tier 3 — make it shippable
- [ ] `just` target that deploys + auto-collapses to one host (kill the manual RS dance).
- [ ] OpenTelemetry traces + structured audit log of auth decisions.
- [ ] Real KV story: TTL semantics, migration, the wasi:keyvalue draft instability.

## Status

- ✅ Contract + impl + infra; e2e on wasmCloud k8s and jco in-process.
- ✅ Config-driven policy via `wasi:config/runtime`.
- ✅ TS examples (HTTP + jco) with passing e2e suites.
- 🚧 Tier 1.
