# jco-lock

Runs the **`lock:mutex@0.1.0`** WASI component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco).

`lock:mutex` is a **distributed advisory mutex** — a lease on a shared store.
A single-process `Mutex` doesn't cross instances; this does. A holder
`acquire`s `key` for a TTL and gets a secret `token` + a monotonic `fence`;
holders `renew` before expiry, and a crashed holder's lease simply lapses (the
TTL is the dead-man's switch), so no lock is ever stuck forever.

- `acquire(key, owner, ttlSeconds)` → a `lease`, or `err(held)` if a live lease
  is held by someone else. An **expired** lease is taken over (fence bumped).
- `release(key, token)` / `renew(token, ttlSeconds)` — token-gated, so a stale
  holder can't release/extend a lease the next holder now owns.
- `holder(key)` — peek the current lease (token blanked; a lapsed lease reads as
  free).

**Fencing token:** `fence` increments each time the lock changes hands. A
resource can reject writes carrying an old fence — so even a paused holder that
wakes after its lease lapsed can't corrupt state.

It is **advisory**: it answers "may I proceed?" and the app honors the answer;
it does not block callers or police the resource.

Imports `wasi:keyvalue/store` + `atomics` (mapped to `src/*-shim.js`),
`wasi:clocks`, and `wasi:random` (the lease token) — the last two from jco.

## Run

```bash
npm install
npm test
```

## What the test proves

acquire → held(other) → release → re-acquire (fence bumps); token-gated
release/renew (wrong token → not-holder); renew keep-alive; expired-lease
takeover; `invalid-ttl`.
