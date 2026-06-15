# Embed feature-flags in-process via jco

The `featureflags:guard` component running **inside the Node process** — no
wasmCloud, no NATS. `jco transpile` turns `feature_flags.wasm` into JS; this
example calls its exported `evaluator` interface directly.

```
feature_flags.wasm        # the built component (copy of components/target/.../feature_flags.wasm)
src/
  keyvalue-shim.js         # host shim for wasi:keyvalue/store  (in-memory Map, overrides)
  config-shim.js           # host shim for wasi:config/runtime  (flag:* definitions)
test/
  featureflags.test.ts     # config bool / rollout / runtime set-rule / tenant scope / list
gen/                       # produced by `jco transpile` (gitignored)
```

## Run

```bash
npm install
npm run transpile          # feature_flags.wasm -> gen/
npm test                   # behavioral checks
```

The two non-standard imports are mapped to local shims at transpile time:

```
jco transpile feature_flags.wasm -o gen \
  --map wasi:keyvalue/store@0.2.0-draft=../src/keyvalue-shim.js \
  --map wasi:config/runtime@0.2.0-draft=../src/config-shim.js
```

Flag definitions live in `config-shim.js` (`flag:{name}` -> `true`/`false` or
`N%`). At runtime, `set-rule(flag, tenant, rule)` writes a rule to the keyvalue
shim — this both DEFINES new flags and overrides config ones; `clear-rule`
removes it. Resolution order is tenant rule > global rule (`tenant = ""`) >
config > false. `list-flags(tenant)` merges all three, reporting each flag's
effective rule and `source`. A percentage rule buckets on a stable hash of
`ctx.subject` so a given subject is sticky.
