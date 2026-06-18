# Embed secrets-vault in-process via jco

The `secrets:vault` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `secrets_vault.wasm` into JS; this
example calls its exported `vault` interface directly.

`secrets:vault` does **envelope encryption**: each secret is sealed with
ChaCha20-Poly1305 (AEAD) under a 32-byte master key the component reads from
`wasi:config` (key `master-key`, base64 STANDARD). Secrets are **versioned** —
`put` bumps the version and keeps old versions retrievable, and `rotate`
atomically writes a new version while reporting the previous one.

```
secrets_vault.wasm    # the built component
src/
  keyvalue-shim.js     # host shim for wasi:keyvalue/store  (in-memory Map)
  config-shim.js       # host shim for wasi:config/runtime  (master-key)
test/
  secrets.test.ts      # put / get / version / rotate / describe / list / delete
gen/                   # transpile output                  (gitignored)
```

## Run

```bash
npm install
npm run transpile      # secrets_vault.wasm -> gen/
npm test               # behavioral checks
```

The two non-standard imports are mapped to local shims at transpile time:

```
jco transpile secrets_vault.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

Swap the in-memory `Map` in `keyvalue-shim.js` for redis/sqlite/NATS to persist;
the component neither knows nor cares — it only sees encrypted blobs.

## The master key

The vault refuses to operate without a valid 32-byte AEAD master key. The
`config-shim.js` supplies one so the example runs hermetically:

```
master-key = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=   # 32 zero bytes
```

This is a **throwaway test key only** — it exists so the tests have a valid key
without prompting. **Real deployments inject the master key via `wasi:config`**
(e.g. the OAM `config:` block on wasmCloud, or a secrets manager), and must use a
real random 32-byte key, never the zero key. Override locally with the
`MASTER_KEY` env var:

```bash
MASTER_KEY="$(node -e "console.log(require('crypto').randomBytes(32).toString('base64'))")" npm test
```

A wrong-length or malformed key surfaces as a `crypto` error from every call.
