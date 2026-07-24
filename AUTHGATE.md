# authgate — TOTP two-factor enrollment + challenge-response login

A **2FA flow**: enroll an account and it mints a TOTP secret (the `otpauth://`
QR an authenticator app scans), **seals it in a vault**, and stays *pending*
until you prove you hold it by typing a first correct code. Activation issues
single-use **recovery codes** (only their hashes are stored). Login verifies a
**live** code — or burns a recovery code — and mints a server-side session.
Chosen because it's the one axis none of the other showcases touch:
authentication as a **challenge-response** — not "send a stored password" but
"prove you hold the shared secret *right now*".

Same shape as the other showcases: one **`mfa-authgate`** HTTP component that
exports `wasi:http` and imports only WIT contracts. The second-factor crypto is
`otp:totp` (RFC 6238 HMAC-SHA1), the secret is envelope-encrypted in
`secrets:vault`, the enrollment QR is rendered by `qr:encode`, and the
post-challenge session is `session:store` — no auth SaaS, no bespoke TOTP, no
plaintext secret in the data store.

![The authgate: enrolling provisions a TOTP secret sealed in the vault (pending) and shows a scannable QR (from qr:encode) plus the secret; a first correct code activates it and reveals single-use recovery codes, then a live code logs in and mints a session — while a wrong code is refused and a recovery code works exactly once — all over one composed wasm component](docs/media/authgate.gif)

## Why it's almost pure composition

| authgate concern | contract | how |
|---|---|---|
| the TOTP secret + code verify + recovery codes | `otp:totp` | `provision(issuer, account)` → secret + QR uri; `verify(secret, code, …)`; `recovery-codes(n)` — holds no state, the secret is supplied per call |
| the scannable enrollment QR | `qr:encode` | `svg(uri, medium, 4)` renders the `otpauth://` URI as an SVG the authenticator app scans — so the user never types the secret |
| sealing the secret | `secrets:vault` | `put(name, secret)` envelope-encrypts under a master key; only ciphertext hits the store; `get` decrypts to verify |
| the post-challenge session | `session:store` | `create(data, ttl)` mints an opaque id + CSRF token; `get` / `revoke` |
| enrollment state + recovery-code hashes | `records:store` | `pending` → `enrolled`; recovery codes stored as SHA-256 hashes (never the codes, never the secret) |

The domain logic is a thin state machine — provision → seal → (first code) →
enroll + issue recovery → (live code) → session. Everything hard (HMAC-SHA1 TOTP,
AEAD envelope encryption, opaque session + CSRF) is the contract.

## The new axis

The others authenticate with a token you already hold or skip auth entirely.
Authgate proves possession **interactively**:

- **challenge-response** — the server stores only the *sealed* secret; a login
  isn't "does this password hash match" but "does the code you just read off
  your phone match the one I derive from the secret at this instant". A wrong or
  stale code is refused; a code from the adjacent 30s window is tolerated
  (clock skew).
- **write-once secret + single-use recovery** — the plaintext secret is
  returned exactly once (to render the QR) and thereafter is read-only-to-verify;
  recovery codes are shown once, stored hashed, and each burns on use. This is
  the only showcase whose headline is *a secret you prove without ever
  re-sending it*.

## Product surface (one component)

```
POST /api/enroll     {account}            provision a secret → QR uri (pending)
POST /api/activate   {account, code}      verify the first code → enrolled + recovery codes
POST /api/login      {account, code}      verify a live TOTP code (or a recovery code) → session
GET  /api/session/{id}                     look up a live session
POST /api/logout     {session}             revoke
GET  /api/status/{account}                 enrolled? recovery codes remaining?
GET  /                                     usage
```

All routes under `/api/…` so the static-dir SPA fallback doesn't shadow them.
No SSE — the flow is request/response.

## Domain model (`records:store`)

- **account** — `{account, state: none|pending|enrolled, recovery: [sha256…],
  at, activated_at}`, indexed by `account`. The TOTP secret lives in the vault
  under `totp/{account}` — never in this record. Recovery codes are stored only
  as their SHA-256 hashes; the plaintext codes exist only in the one activation
  response.

## Component map

**Reused as-is (5):** `otp:totp` (the second-factor primitive), `qr:encode` (the
scannable enrollment QR), `secrets:vault` (the sealed secret), `session:store`
(the post-challenge session + CSRF), and `records:store` (enrollment state +
recovery hashes). Plus host WASI:
`wasi:clocks/wall-clock`, `wasi:keyvalue`, `wasi:config` (the vault master key).
This showcase is the first app to drive `otp:totp` through its full
provision → verify → recovery lifecycle.

**New (1):** `mfa-authgate` — `mfa:app` exports `wasi:http`. The 2FA state
machine (provision → seal → activate → challenge) + session issuance.

**Not used:** `auth-guard` — this app *is* the authentication factor, not a
consumer of one; it pairs *beside* auth-guard (password) as the second factor,
rather than importing it.

## Build order (each rung is demoable)

1. **Enroll + seal** — `POST /api/enroll` over `otp:totp` + `secrets:vault`.
   `just e2e-authgate` provisions a secret, derives a real RFC-6238 code from it,
   and asserts the account is `pending` with the secret sealed in the vault.
2. **Activate + recovery** — a first correct code flips to `enrolled` and issues
   recovery codes; e2e proves a wrong first code is refused and re-activation is
   a 409.
3. **Challenge login + session + browser UI** — a live code (or a recovery code)
   mints a `session:store` session; the SPA walks enroll → activate → login.
   e2e proves a wrong code is rejected, a recovery code is **single-use**, and
   logout revokes the session. `just host-authgate`, enroll and log in.
4. **Bench** — the crypto dimension: TOTP verify throughput and vault
   seal/unseal latency per login. See `bench/AUTHGATE-BENCH.md`.

## Non-goals (v1)

Password / first-factor auth (that's `auth-guard` — this is the *second* factor),
rate-limiting the code endpoint (compose `ratelimit:guard` in front — see the
ratelimit showcase), WebAuthn / passkeys, and push-based approval. The showcase
demonstrates the **TOTP challenge-response composition**, not a full IdP.
