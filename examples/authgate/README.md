# authgate — TOTP 2FA + challenge-response login (e2e)

The [docs/apps/AUTHGATE.md](../../AUTHGATE.md) showcase as one composed wasm HTTP component
on the native Rust host, plus a browser SPA. The challenge-response axis: the
server stores only the *sealed* TOTP secret; a login proves you hold it right
now, it never re-sends it.

## Run it

```bash
just host-authgate    # from repo root; authgate on http://127.0.0.1:3023
```

**Enroll** an account (copy the secret / scan the QR into an authenticator app),
**activate** with the first code it shows (revealing single-use recovery codes),
then **log in** with a live code — or burn a recovery code. `CFG_MASTER_KEY`
(32-byte base64) is the vault master key that seals the secret.

## Test it

```bash
just e2e-authgate     # composes + builds host + runs tests/authgate.rs
```

The test derives real RFC-6238 codes from the returned secret (via `totp-lite`),
exactly as an authenticator app would, and proves: enroll → `pending`; a wrong
first code is refused; a correct code → `enrolled` + 5 recovery codes; login via
a live code mints a session; a wrong login code is rejected; a recovery code
logs in **once** (reuse fails); logout revokes the session.

## What's composed

`mfa-authgate` (`mfa:app`) imports only contracts:

- `otp:totp` — provision the secret + QR uri, verify codes, mint recovery codes
- `secrets:vault` — envelope-encrypt the TOTP secret (only ciphertext stored)
- `session:store` — the opaque post-challenge session + CSRF token
- `records:store` — enrollment state + recovery-code hashes

plus host WASI: `wasi:keyvalue`, `wasi:config` (vault master key), `wasi:clocks`.
No `auth-guard` — this app **is** the second factor.
