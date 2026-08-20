# triage:assist — the contract

Authenticated, rate-limited defect intake with an AI severity assist and an audit
ledger. One component, three parts, written at the same time by three agents.

This document is the specification. It is not writable: if something here is wrong
or missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and
the other parts answer between generations.

## The shape of the whole

```
POST /api/reports ──▶ intake ──▶ authorize ─▶ rate limit ─▶ REDACT ─▶ store
                                     │
POST /api/reports/{id}/assist ─▶ assist ──▶ reads the STORED body ─▶ model
                                     │
GET  /api/audit ──────────────▶ ledger ──▶ what all three wrote down
```

The ordering is the point. `assist` sends the model what `intake` **stored**, never
what the request said — a redaction that happens after the model call is a leak that
nothing in the response reveals. `ledger` is how any of it can be shown afterwards.

## Tenancy, identity, and how a caller gets a token

Every `/api/*` route needs `Authorization: Bearer <token>`. A part does **not** parse
the token: `auth:identity/authorizer.authorize(token, permission)` verifies it and
checks the permission in one call, and returns the `principal`.

A scope named `"<target>:<action>"` grants the permission `{target, action}`, so
these are the two permissions this API uses:

| route | permission | scope that grants it |
|---|---|---|
| `POST /api/reports`, `POST …/assist` | `{target:"reports", action:"write"}` | `reports:write` |
| `GET /api/reports…`, `GET /api/audit` | `{target:"reports", action:"read"}` | `reports:read` |

The router mints tokens at `POST /test/token` so no gate has to log in through a
part it is not judging. `principal.subject` is the caller, and it is the rate-limit
key and the audit subject.

Failures, and they are three different answers — a part that collapses them into one
fails its gate:

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

## Storage

One collection, `reports`, indexed on `component` and `state`. The stored document:

```json
{
  "title": "Login fails, silently",
  "body": "reached me at [EMAIL] — no error is shown",
  "component": "auth",
  "state": "open",
  "reporter": "ada",
  "reported_at": "2026-08-20T09:00:00Z",
  "assist": {
    "severity": "major",
    "confidence": 780,
    "summary": "Login fails without showing the user an error.",
    "assisted_at": "2026-08-20T09:01:00Z"
  }
}
```

`assist` is absent until the report has been assisted. `reporter` is
`principal.subject`. Timestamps are RFC3339 UTC seconds, from
`wasi:clocks/wall-clock`.

## Part 1 — `intake` (`src/intake.rs`)

### `POST /api/reports` — requires `reports:write`

Body: `{"title": string, "body": string, "component": string}`. All three required
and non-empty; anything missing is `400 {"error":"invalid_report"}`.

**What is stored is masked.** A reporter pastes their email and phone number into a
bug report constantly, and that body goes to a model and into a digest anyone can
read. `pii:redact` masks it *before* it reaches the store, and the raw text is never
written:

```rust
use crate::bindings::pii::redact::redactor as pii;

pii::redact(text: &str, opts: pii::Options) -> String
pub struct Options { pub kinds: Vec<Kind> }   // EMPTY kinds = every kind
pub enum Kind { Email, CreditCard, Ssn, Phone, Ip }
```

**The rate limit takes two calls, and this is what everybody gets wrong.**
`ratelimit:guard` is a fixed-window *failure counter*, not a throughput meter. It
counts what you tell it to count, so an accepted report is an attempt you must
record:

```rust
use crate::bindings::ratelimit::guard::limiter as rl;

rl::check(key: &str)          -> Result<u32, LimitError>  // remaining, or Locked(secs)
rl::record_failure(key: &str) -> Result<(), LimitError>   // one attempt spent
```

So: `check` first — `Err(Locked(secs))` is `429 {"error":"rate_limited","retry_after":secs}`
and nothing is stored — then `record_failure` once the report is accepted. The key is
`principal.subject`. `Err(BackendUnavailable(_))` from either call is
`503 {"error":"rate_limit_unavailable"}`: a limiter that is down must not silently
become no limiter.

Success is `201 {"id": "<report id>"}`. The id is the one `records:store` minted —
`create` returns an `Entry` whose `id` is a fresh ULID, so nothing here generates one.

### `GET /api/reports/{id}` — requires `reports:read`

`200` with the stored document, `404 {"error":"not_found"}`.

### `GET /api/reports?component=&state=` — requires `reports:read`

`200 {"reports":[{"id": …, …document…}, …]}`. Both filters optional, and both are
`records:store` index lookups rather than a full scan — `find_by` wants the value
**JSON-encoded** (`"\"auth\""`, not `auth`), and a wrong query returns `Ok(vec![])`,
which is indistinguishable from an empty collection.

## Part 2 — `assist` (`src/assist.rs`)

### `POST /api/reports/{id}/assist` — requires `reports:write`

Reads the report **from the store** and asks the model about the stored `title` and
`body`. Two calls, both on `ai:inference/inference`:

```rust
use crate::bindings::ai::inference::inference as ai;

ai::classify(text: &str, labels: &[String]) -> Result<LabelScore, AssistError>
ai::summarize(text: &str, len: ai::Length, focus: &str) -> Result<String, AssistError>
pub struct LabelScore { pub label: String, pub confidence: u32 }   // 0..=1000 MILLI-units
```

* `classify` with exactly `["critical", "major", "minor"]`. A label outside that set
  is `502 {"error":"unexpected_severity"}` — the model is not trusted to answer
  outside the menu it was given.
* `confidence` is stored and answered **exactly as `classify` returned it**, which is
  0..=1000 milli-units and not a percentage. Dividing it by ten to make it look like
  one loses the only precision the interface offers, and the field then means two
  different things depending on who wrote the caller.
* `summarize` with `Length::Brief` and the focus `"what is broken and where"`.
* The text you send is `title` + `"\n"` + the stored (masked) `body`. Not the
  request body: the request body of *this* route is empty.

Success is `200 {"severity","confidence","summary"}`, and the same four fields
(`assisted_at` added) are written into the report document under `assist`.

| condition | status | body |
|---|---|---|
| no such report | 404 | `{"error":"not_found"}` |
| already has an `assist` | 409 | `{"error":"already_assisted","severity":"<the stored one>"}` |
| either call returns `AssistError` | 503 | `{"error":"assist_unavailable"}` |

A `503` leaves the report **exactly as it was** — no half-written `assist`, no
`assisted_at`, no severity. A provider that is down is a provider that is down; it is
not a report with an empty opinion attached.

### `GET /api/reports/{id}/assist` — requires `reports:read`

`200` with the stored assist, `404 {"error":"not_assisted"}` when there is none, and
`404 {"error":"not_found"}` when there is no such report.

## Part 3 — `ledger` (`src/ledger.rs`)

The audit trail, and the signature the other two parts call. **This function is the
protocol** — all three parts compile against it, and a change to its shape breaks the
join:

```rust
pub fn note(trace: &str, event: &str, outcome: &str, subject: &str, detail: &str)
```

It writes one `audit:log/recorder.record_event`, filling the record from its
arguments: `tenant` `"triage-assist"`, `span_id` `""`, and `trace_id` from `trace`.

**`id` and `timestamp` are not yours to invent.** `audit-log` mints the event id when
`id` is empty and stamps `now()` when `timestamp` is zero — so pass `String::new()`
and `0` and let it. That is not documented in the signature, only in the interface's
own comments, and a part that generates its own id here is doing work the capability
already did. It
returns nothing and must never fail a request: an audit backend that is down is a
`note` that did nothing, not a 500 on the caller's report.

The events the other parts write, and they are named exactly this:

| what happened | `event` | `outcome` |
|---|---|---|
| a report was accepted | `reports.create` | `ok` |
| a report was refused for auth | `reports.create` | `denied` |
| a report was refused for the rate limit | `reports.create` | `throttled` |
| a model answered | `reports.assist` | `ok` |
| the model was unavailable | `reports.assist` | `error` |

The router additionally notes every dispatched request as `http.request` / `ok`, so
the ledger has traffic to show before any other part exists.

### `GET /api/audit?limit=N` — requires `reports:read`

`200 {"events":[…]}`, newest first, `limit` default 20 and capped at 100, over
`audit:log/query.recent`. Each event is the record's ten fields, camel-free, exactly
as the interface names them (`trace_id`, `span_id`, …).

### `GET /api/audit?trace=T` — requires `reports:read`

The same shape, but only that trace's events, over `audit:log/query.by_trace`. `T`
comes from the `traceparent` request header the router extracts and passes to `note`;
a request without one gets a generated trace id.

## The router, which no part may write

`src/lib.rs` dispatches, and also owns:

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` — how the harness tells "not up" from "wrong" |
| `POST /test/token` | mints a token: `{"subject": "...", "scopes": ["reports:write"]}` → `201 {"token": "..."}` |
| `POST /test/seed` | writes two reports straight to the store, unassisted, one carrying an email — so `assist` and `ledger` can be judged before `intake` exists |
| `GET /test/report/{id}` | the stored document, raw — so a part is judgeable on what it WROTE without the part that owns the read route |

## How you are judged

By real HTTP requests against the running component, and by what the compiled
component **imports**. `cargo component check` passes happily on code that does
nothing, and a hand-rolled email scanner answers 201 on a well-behaved body exactly
like the real one — so each gate also reads which capabilities the artifact actually
calls. Writing your own PII masker, your own token parser, your own counter or your
own model client is the one way to fail a gate while answering every request
correctly.
