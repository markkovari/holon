# jco i18n example

Runs the `i18n:catalog` WebAssembly component **in-process** under Node via
[`jco`](https://github.com/bytecodealliance/jco). Demonstrates a message
catalog with:

- **Interpolation** — `translate("en", "greeting", [{name:"name", value:"Al"}])`
  renders `Hello, {name}!` -> `Hello, Al!`.
- **Plurals** — `setPlural` registers CLDR-style categories
  (`one`/`other`/...); `translatePlural` picks the form for a count and
  auto-injects `{count}`.
- **Locale fallback** — `en-US` falls back to base `en`; an unknown locale
  falls back to the configured `default-locale`.
- **Negotiation** — `negotiate(preferred, available)` picks the best match
  (exact, then base language, then default).

## Host shims

The component imports two non-standard WASI interfaces; jco supplies the rest.
Both shims are trivial and swappable for real backends:

- `src/keyvalue-shim.js` — `wasi:keyvalue/store` over an in-memory `Map`. Swap
  for redis/sqlite/NATS without touching the component.
- `src/config-shim.js` — `wasi:config/runtime`. Exposes `default-locale`
  (override via `DEFAULT_LOCALE` env var).

## Run

```bash
npm install
npm test        # transpiles i18n_catalog.wasm -> gen/, then runs the test suite
```

`npm run transpile` alone regenerates `gen/` with the two shims mapped in.
