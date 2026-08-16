# report — batch CSV import → typed validate → paged report → CSV export

A **CSV import tool**: paste a spreadsheet, and the round-trip runs entirely over
contracts — every row is **parsed to typed fields**, **validated** against a
field-rule set (types, required, ranges, one-of), and the clean rows persist
while the rejected ones come back with **per-field errors** you can act on. Read
the clean set back through an **opaque cursor**, then **export it to CSV** with
the same codec that parsed it. Chosen because it's a **batch-ingest + read-back**
axis: not one request in and one out, but a whole file split into accepted and
rejected, then round-tripped.

Same shape as the other showcases: one **`csv-report`** HTTP component that
exports `wasi:http` and imports only WIT contracts. Parsing is `csv:codec`,
validation is `validate:schema`, paging is `paginate:cursor` — no CSV crate in
the domain component, no hand-rolled validator, no offset math.

![The report tool: pasting a CSV imports the valid rows and lists the rejected ones with per-field errors (bad email, age out of range, unknown role), pages the clean set through an opaque cursor, and exports it back to CSV — all over one composed wasm component](../media/report.gif)

## Why it's almost pure composition

| report concern | contract | how |
|---|---|---|
| parse the pasted CSV into typed records | `csv:codec` | `parse-records(text, dialect)` → `record-row{pairs}`; `format(rows, dialect)` re-serializes on export |
| per-row typed validation | `validate:schema` | `validate(json, rules)` → `field-error{field, code, message}`; empty = accept |
| the clean rows | `records:store` | accepted rows persist here, indexed by `email`, counted for stats |
| opaque "load more" paging | `paginate:cursor` | `clamp-limit(n)` bounds the page; the store's continuation is wrapped in `encode`/`decode` so the client never sees an offset |

The domain logic is a thin pipeline — parse → coerce each cell to the type its
rule declares → validate → split accepted/rejected → store the clean ones.
Everything hard (CSV quoting, email/uuid/range checks, cursor signing) is the
contract.

## The new axis

The others take one thing and act on it. Report takes a **batch** and proves the
split + the round-trip:

- **accept/reject split** — a single import returns `{imported, rejected,
  rejects[]}`, and each reject carries the **line number and the failing
  fields** (`email: not a valid email address`, `age: expected 0..130`,
  `role: must be one of admin, user, guest`). A bad upload is *diagnosable*, not
  just refused.
- **round-trip** — the same `csv:codec` that parsed the input re-serializes the
  clean set on `GET /api/export`, and the report is paged through
  `paginate:cursor`. This is the only showcase whose headline is a *file in, a
  clean file out* — validation is the thing in the middle.

## Product surface (one component, anonymous)

```
GET  /api/schema                     the active field-rule set
POST /api/import       (CSV body)    parse → validate → store; returns {imported, rejected, rejects[]}
GET  /api/rows         ?limit=&after=   paged clean rows (opaque cursor)
GET  /api/export                     the clean set re-serialized to text/csv
GET  /api/stats                      stored-row count
GET  /                               usage
```

All routes under `/api/…` so the static-dir SPA fallback doesn't shadow them
(same rule as search/pulse/flags). No SSE — report is request/response; the
*new* thing is the batch split + round-trip, not a stream.

## Domain model (`records:store`)

- **row** — the coerced, validated record, e.g.
  `{name, email, age, role, at}`. CSV is all strings on the wire; each cell is
  coerced to the JSON type its rule declares (`integer`/`number`/`boolean`)
  *before* validation, so an `age` of `"999"` is checked as the number `999`
  against the `0..130` range, and a cell that won't coerce stays a string so the
  validator reports the type error itself. Rows persist indexed by `email`.

## Component map

**Reused as-is (4):** `csv:codec` (parse + format — the round-trip), `validate:schema`
(the typed field rules), `records:store` (the clean rows), and `paginate:cursor`
(the opaque report cursor). Plus host WASI: `wasi:clocks/wall-clock` (`at`). This
showcase is the read-heavy user of both `csv:codec` and `validate:schema`.

**New (1):** `csv-report` — `report:app` exports `wasi:http`. The ingest pipeline
(parse → coerce → validate → split → store) + the paged read-back and CSV export.

**Not used:** `auth-guard` (anonymous import tool), and anything stream/SSE (this
is the request/response one).

## Build order (each rung is demoable)

1. **Parse + validate** — `POST /api/import` over `csv:codec` + `validate:schema`.
   `just e2e-report` imports a mix of valid and invalid rows and asserts the
   split: 3 imported, 2 rejected, with the **per-field errors** surfaced
   (bad email, age over range, unknown role).
2. **Paged report** — `GET /api/rows` over `paginate:cursor`; e2e walks the clean
   set in pages of two through the opaque cursor and asserts a garbage cursor is
   a 400, not silently ignored.
3. **CSV export + browser UI** — `GET /api/export` re-serializes through the same
   codec (round-trip); the SPA is a paste-box that shows imported/rejected
   tallies, the reject list with per-field errors, the paged table, and a
   download button. `just host-report`, paste a CSV and watch it split.
4. **Bench** — the batch dimension: rows-per-second through parse+validate+store
   for a large CSV, and validate-only vs full-pipeline overhead. See
   `bench/REPORT-BENCH.md`.

## Non-goals (v1)

Streaming multi-megabyte uploads (the whole CSV is read into memory — the
contract is `parse(text)`), per-tenant configurable schemas (the rule set is
fixed in the component for the demo), and column type inference (the schema is
declared, not guessed). The showcase demonstrates the **ingest composition**,
not a data-warehouse loader.
