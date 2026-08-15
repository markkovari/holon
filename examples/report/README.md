# report — batch CSV import/report (e2e)

The [docs/apps/REPORT.md](../../REPORT.md) showcase as one composed wasm HTTP component on
the native Rust host, plus a browser SPA. The batch-ingest axis: parse a CSV,
validate every row against a typed field-rule set, store the clean ones, and
round-trip the set back out to CSV.

## Run it

```bash
just host-report      # from repo root; tool on http://127.0.0.1:3022
```

Open the page, paste a CSV (a sample is pre-filled), and **Import**: valid rows
land in the paged report, rejected rows come back with **per-field errors**
(bad email, age out of range, unknown role). Page the clean set through the
opaque cursor, then **Export** it back to CSV.

## Test it

```bash
just e2e-report       # composes + builds host + runs tests/report.rs
```

Proves: a mixed CSV splits into imported vs rejected with per-field errors;
the clean set pages through the opaque cursor (and a garbage cursor is a 400);
export re-serializes through the **same codec** that parsed the input.

## What's composed

`csv-report` (`report:app`) imports only contracts:

- `csv:codec` — parse the upload + format the export (the round-trip)
- `validate:schema` — per-row typed validation
- `records:store` — the clean rows
- `paginate:cursor` — the opaque report cursor

plus host WASI (`wasi:keyvalue`, `wasi:config`, `wasi:clocks`). No auth.
