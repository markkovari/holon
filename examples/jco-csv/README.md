# jco-csv

Exercises the `csv:codec` component in-process via [jco](https://github.com/bytecodealliance/jco).

The component is an RFC 4180 CSV parser/formatter:

- `parse(text, opts)` — split a document into rows of fields, with quoted
  fields (embedded commas, quotes, and newlines), optional whitespace trimming,
  and a configurable delimiter (e.g. `\t` for TSV).
- `parseRecords(text, opts)` — when `hasHeader` is set, pair each header column
  with its row value; a data row with the wrong arity throws `ragged-row`.
- `format(rows, opts)` — render rows back to CSV text, quoting any field that
  would otherwise be ambiguous, so `format` -> `parse` round-trips cleanly.

This is **pure compute**: the component needs no WASI host imports, so the
example transpiles with no `--map` shims.

## Run

```bash
npm install
npm test
```

`npm test` first runs `jco transpile csv.wasm -o gen`, then executes the
`node:test` suite in `test/csv.test.ts` with `tsx`.
