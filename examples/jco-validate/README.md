# jco-validate

Exercises the `validate:schema` component **in-process** via
[jco](https://github.com/bytecodealliance/jco).

The component does **declarative input validation**: you hand it a JSON string
and a list of field rules, and it returns the rules that failed. It is
**pure-compute** — no WASI imports to satisfy, so jco transpilation needs **no
shims** and the resulting JS module runs directly under Node.

## What it checks

Each `Rule` describes one field: its `kind`
(`text` | `integer` | `number` | `boolean` | `email` | `alphanumeric` | `uuid`),
whether it is `required`, length bounds (`min-len`/`max-len`), numeric bounds
(`min-value`/`max-value`, both `option<f64>`), and an optional `one-of` enum.

`validate(json, rules)` returns a `field-error[]` — empty when everything
passes. Failure codes include `required`, `type`, `min-len`, `format`,
`max-value`, `one-of`. Non-object JSON yields a single `format` error on
field `""`.

## Run

```bash
npm install
npm test
```

`npm test` first transpiles `validate.wasm` to `gen/` (`jco transpile
validate.wasm -o gen`) and then runs `test/validate.test.ts` with the Node test
runner via `tsx`.
