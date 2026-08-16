# conduit — the RealWorld spec on the native Rust host

The full [RealWorld](https://realworld-docs.netlify.app/) ("Conduit") backend as
**one composed wasm HTTP component** — `conduit-domain` + auth-guard +
record-store + slug — served by the native Rust host (`host/`, wasmtime). No
Node, no jco. See [`../../docs/apps/CONDUIT.md`](../../docs/apps/CONDUIT.md) for the full design.

Users & profiles, articles (CRUD + slug + filters + feed + tags), comments,
favorites — the complete API, composed from contracts with no bespoke business
crate.

![The official RealWorld Hurl conformance suite going green against the composed app: 13/13 files, 154 requests](../../docs/media/conduit-conformance.gif)

## Verify

```bash
just conformance-conduit   # OFFICIAL RealWorld Hurl suite → 13/13 files green
just e2e-conduit           # Rust e2e (ureq) spawns the host, drives the API
just host-conduit          # serve it yourself on http://0.0.0.0:3008
```

`just conformance-conduit` is the headline: it runs the upstream RealWorld Hurl
suite (vendored in [`conformance/`](conformance)) against the running app — an
external, objective check anyone can reproduce. Needs [`hurl`](https://hurl.dev).

```bash
curl -s localhost:3008/api/users -d '{"user":{"username":"jane","email":"jane@rw.test","password":"password1"}}'
```

## What runs where

| Layer | Language |
|---|---|
| `conduit-domain` (routing + RealWorld JSON shape) | Rust → wasm |
| auth-guard, record-store, slug (behind it) | Rust → wasm |
| host serving the composed `.wasm` | Rust (`host/`, wasmtime) |
| e2e (`ureq`) | Rust |
| conformance (`hurl`) | vendored upstream suite |
