# jco-pii

Exercises the `pii:redact` component in-process with [jco](https://github.com/bytecodealliance/jco).

The component detects and obscures personally identifiable information:

- **`detect(text, opts)`** — returns `{ kind, start, length }` findings.
- **`redact(text, opts)`** — replaces matches with tokens (`[EMAIL]`, `[CARD]`, `[SSN]`, `[PHONE]`, `[IP]`).
- **`mask(text, opts)`** — partially masks matches, keeping enough to stay recognisable (e.g. `j***@e***.com`, `**** **** **** 4242`).

Supported kinds: `email`, `credit-card` (Luhn-checked), `ssn`, `phone`, `ip`.
Pass `opts.kinds: []` to cover every kind, or list specific kinds to filter.

Detection is **regex-free** — matching and the Luhn check run inside the
component. It is **pure-compute**: no WASI host shims are wired in, so jco
transpiles and runs the component directly.

Pairs naturally with `audit:log`: scrub PII out of payloads before they hit an
append-only audit trail.

## Run

```bash
npm install
npm test
```

`npm test` transpiles `pii_redact.wasm` into `gen/` (via `jco transpile`) and
runs the `node:test` suite over the generated bindings.
