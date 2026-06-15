# Embed blob-store in-process via jco

The `blob:store` component running **inside the Node process** — no wasmCloud, no
NATS. `jco transpile` turns `blob_store.wasm` into JS; this example calls its
exported `blobstore` interface directly.

Large-object storage organized into named containers: `put`/`get`/`head`/
`exists`/`delete`/`list-objects` over whole-body `Uint8Array`s. Where keyvalue is
for small values and cache for hot ephemeral data, this is for arbitrary binary
objects (uploads, exports, attachments).

```
blob_store.wasm       # the built component
src/
  keyvalue-shim.js     # host shim for wasi:keyvalue/store  (in-memory Map)
test/
  blob.test.ts         # put/get, head, not-found, delete, container isolation, prefix list, binary names
gen/                   # produced by `jco transpile` (gitignored)
```

## Run

```bash
npm install
npm run transpile      # blob_store.wasm -> gen/
npm test
```

Only the non-standard `wasi:keyvalue/store` is mapped to the local in-memory
shim. Each object is stored as two kv entries — `bo_{container}/{name}` (bytes)
and `bm_{container}/{name}` (size + content-type) — with both parts sanitized so
`/` and `_` in container/object names round-trip safely. A deployment binds the
kv import to a real object backend (S3 / R2 / GCS / filesystem provider); the
component never knows which.

> Whole-body `list<u8>` in/out is wasip2-stable. Chunked / streamed reads for
> genuinely huge objects are a future wasip3 async revision.
