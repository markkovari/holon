# RealWorld conformance suite (vendored)

`hurl/` is the **official** RealWorld API test suite, pinned from
[`gothinkster/realworld`](https://github.com/gothinkster/realworld) `specs/api/hurl`.
It is vendored (not fetched at run time) so conformance is reproducible offline
and the suite version is fixed — upstream has already moved once (it retired the
Postman/newman collection in favour of Hurl + Bruno).

Run it against the composed conduit app on the native Rust host:

```bash
just conformance-conduit        # from repo root: build + compose + host + hurl
# or, against an already-running host:
HOST=http://127.0.0.1:3008 bash run.sh
```

`run.sh` starts `comp-host` with the composed `conduit_domain` wasm (in-memory KV,
so each run is clean), waits for readiness, and runs `hurl --test` over every
`hurl/*.hurl` file with `host` + `uid` variables. Requires [`hurl`](https://hurl.dev).

Result: **13/13 files, 154 requests green.** Update the vendored suite by copying
newer `*.hurl` files from upstream `specs/api/hurl`.
