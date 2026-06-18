# jco-otp — `otp:totp` in-process via jco

Exercises the `otp:totp` WebAssembly component from Node, transpiled to JS with
[jco](https://github.com/bytecodealliance/jco) and called in-process.

## What it does

The component implements **TOTP (RFC 6238)** and **HOTP (RFC 4226)** over
HMAC-SHA1 with base32 secrets, plus provisioning and recovery-code helpers:

| Function | Purpose |
|----------|---------|
| `provision(issuer, account)` | Generate a random secret + `otpauth://` enrolment URI |
| `totpAt(secret, timestamp, period, digits)` | TOTP for a given Unix time |
| `totpNow(secret)` | TOTP for the current wall-clock time |
| `verify(secret, code, period, digits, skew)` | Validate a code within `±skew` steps |
| `hotpAt(secret, counter, digits)` | Counter-based HOTP |
| `recoveryCodes(count)` | Generate single-use backup codes |

## Stateless by design

The component holds **no state**: the caller supplies the shared `secret` on
every call. In production, store secrets in a `secrets:vault` component, not in
the OTP component itself.

## Auto-shimmed imports

The component imports only `wasi:clocks` (for `totp-now`) and `wasi:random`
(for `provision` / `recovery-codes`). Both are **auto-shimmed by jco** via
`@bytecodealliance/preview2-shim`, so this example needs **no custom shims** and
no `--map` flags.

## Run

```bash
npm install
npm test        # transpiles otp.wasm -> gen/, then runs the test suite
```

The suite asserts the official **RFC 6238 / RFC 4226 known-answer vectors**,
which prove the crypto is byte-for-byte correct.
