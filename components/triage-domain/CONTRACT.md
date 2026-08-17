# The triage API — the interface three parts build against

One component serves all of it. Three parts write it: **intake**, **workflow** and
**digest**. None may edit this file; if it is wrong or missing something, write
`CONTRACT-REQUEST.md` — first line the subject, the rest why — and the other parts
will answer at the next generation.

All bodies are JSON except where it says otherwise. All ids are strings. Times are
RFC3339 UTC (`2026-08-17T09:00:00Z`).

## The report document

Every part reads or writes this, and it is the one shape all three must agree on. It
lives in `records:store` under the collection **`reports`**, indexed on
**`component`** and **`state`**:

```json
{
  "title":       "Login fails, silently",
  "body":        "contact me at [EMAIL]",
  "component":   "auth",
  "state":       "open",
  "severity":    "high",
  "reported_at": "2026-08-17T09:00:00Z"
}
```

- `state` is one of `open`, `triaged`, `fixed`, `closed`.
- `severity` is **absent until triaged**, then one of `low`, `medium`, `high`.
- `body` is stored **masked** — see `intake`.
- The record's id is the store's, from `create`. It is the report id everywhere.

## Intake — owned by the `intake` part

```
POST /api/reports        {title, body, component}   201 {id, title, component, state, body} | 400
GET  /api/reports/{id}                              200 {id, …the document} | 404
GET  /api/reports?state=&component=                 200 {reports:[…]}
```

- `title`, `body` and `component` are all required and non-empty → otherwise **400**
  `{"error":"invalid"}`.
- A new report is created with `state` `open` and **no** `severity`.
- **`body` is stored with PII masked**, using `pii:redact`. A body of
  `contact me at ada@example.test` is stored and returned as
  `contact me at [EMAIL]`. The raw text is never stored.
- `GET /api/reports` filters by `state` and/or `component` when given, and returns
  everything when not. Both filters together are an AND.
- A **duplicate** is a 409: same `component` AND same `title` as an existing report
  that is not `closed` → **409** `{"error":"duplicate","existing":"<report id>"}`.
  A closed report does not block a new one — the bug came back.

## Workflow — owned by the `workflow` part

```
POST /api/reports/{id}/transition   {event, severity?}   200 {id, state, severity?} | 400 | 404 | 409
GET  /api/queue                                          200 {queue:[…]}
```

The lifecycle, and it is a `fsm:workflow` **definition** — not a chain of string
comparisons:

```
open   --triage--> triaged
triaged --fix----> fixed
fixed  --close---> closed
triaged --close---> closed      (won't fix)
open   --close---> closed       (not a bug)
```

`closed` is terminal. Register the machine under the name **`report`**, with
`open` as the initial state and the instance id equal to the report id.

- `event` is required and must be one of `triage`, `fix`, `close` → otherwise
  **400** `{"error":"invalid"}`.
- An unknown report id is **404** `{"error":"not_found"}`.
- An event that is not legal from the report's current state is **409**
  `{"error":"illegal","state":"<the current state>"}`. The current state comes from
  the fsm's own error, which carries it.
- The `triage` event **requires** `severity` in the body, one of `low`, `medium`,
  `high` → otherwise **400** `{"error":"invalid"}`. It is stored on the document.
  Every other event ignores `severity`.
- A successful transition updates **both**: the fsm instance, and the `state` field
  on the report document in `records:store`. `digest` reads the document and must
  not have to ask the fsm.
- `GET /api/queue` returns the reports that are **not** `closed`, most urgent first:
  `high` before `medium` before `low`, reports with no severity **last**, and within
  the same severity the older `reported_at` first. Each entry is
  `{id, title, component, state, severity}` — `severity` omitted when absent.

## Digest — owned by the `digest` part

```
GET /api/digest?day=YYYY-MM-DD        200 {day, total, by_state:{}, by_component:{}, open_high}
GET /api/digest.csv?day=YYYY-MM-DD    200 text/csv
```

- Both need `day`; missing or unparseable is a **400** `{"error":"invalid"}`.
- `day` selects reports whose `reported_at` **starts with** that date.
- `total` is how many that day. `by_state` maps state → count and `by_component`
  maps component → count, **both including only states/components that occur**.
  `open_high` counts reports that day with `severity` `high` and `state` not
  `closed`.
- The CSV has a header row, exactly these columns in this order:
  `id,title,component,state,severity` — then one row per report that day, sorted by
  `reported_at` ascending, then by `id`. A day with no reports is the header alone.
  An absent `severity` is an **empty field**, not the word `null`.
- **Use `csv:codec` to format it**, and answer with `Reply::raw(200, "text/csv", …)`.
  One seeded title contains a comma, so a field with a comma in it must come back
  quoted or the row stops having five columns. The gate counts columns.

## Shared by all

- Unknown route → **404** `{"error":"not_found"}`.
- Malformed JSON → **400** `{"error":"invalid"}`.
- `GET /health` → **200** `{"ok":true}` (already written; do not change it).
- `POST /test/seed` → **201** `{report_ids:[…]}` (already written; a fixture, so a
  part can be judged before the part upstream of it exists).

## Storage

`records:store` collection: `reports`, indexed on `component` and `state`. Ids come
from the store. Records are JSON documents; the shape above is what a reader expects
to find in them.
