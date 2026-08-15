# drop — presigned direct-upload drop-box (e2e)

The [docs/apps/DROP.md](../../docs/apps/DROP.md) showcase as one composed wasm HTTP component on the
native Rust host, plus a browser SPA. The presigned axis: the backend answers
the policy question and signs a ticket — it never proxies the upload.

## Run it

```bash
just host-drop        # from repo root; drop-box on http://127.0.0.1:3021
```

Open the page, pick a file, and watch the three steps: **① ticket** (the policy
answer, no bytes), **② PUT** the bytes straight to storage, **③ signed download
link**. A blocked content-type (default gate allows `text/plain,image/png`) or
an oversize file is refused at ticket time. `CFG_ALLOWED_TYPES` / `CFG_MAX_SIZE`
tune the gate.

## Test it

```bash
just e2e-drop         # composes + builds host + runs tests/drop.rs
```

Proves: a disallowed type and an oversize request are both refused at ticket
time (no bytes uploaded); a redeemed ticket stores the bytes; a signed download
link round-trips the exact bytes; a **tampered signature is refused (403)**.

## What's composed

`upload-drop` (`drop:app`) imports only contracts:

- `upload:policy` — the gate + signed, expiring ticket
- `blob:store` — the object bytes
- `webhook:sign` — the download-link HMAC
- `records:store` — object metadata + the id that keys the blob

plus host WASI (`wasi:keyvalue`, `wasi:config`, `wasi:clocks`). No auth.
