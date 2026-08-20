# docsearch:agent — the contract

Retrieval-augmented answers over an indexed library, behind a TOTP step-up, a
per-subject budget and a cache. One component, three parts, written at the same time
by three agents.

This document is the specification. It is not writable: if something here is wrong or
missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and the
other parts answer between generations.

## The shape of the whole

```
POST /api/docs ───────────▶ library ──▶ index + store
POST /api/mfa/verify ─────▶ stepup ───▶ marks the session stepped-up
POST /api/answer ─────────▶ answer ───▶ step-up? ─▶ cache? ─▶ quota ─▶ search ─▶ model
```

**The hard part is what a second identical question must cost: nothing.** An answer
served from the cache spends no budget and reaches no model. A refusal — no sources,
no budget, no step-up — is never cached as though it were an answer. Neither property
is visible in a single request, which is why the gate asks twice.

## Identity, and the second factor

Every `/api/*` route needs `Authorization: Bearer <token>`, resolved by the part with
one call:

```rust
use crate::bindings::auth::identity::authorizer as authz;
authz::authorize(token: &str, required: &Permission) -> Result<Principal, AuthError>
```

A scope `"<target>:<action>"` grants `Permission { target, action }`.

| route | permission | scope |
|---|---|---|
| `POST /api/docs` | `{docs, write}` | `docs:write` |
| `GET /api/docs/{id}`, `GET /api/search` | `{docs, read}` | `docs:read` |
| `POST /api/answer` | `{docs, read}` **and a stepped-up session** | `docs:read` |
| `POST /api/mfa/enroll`, `POST /api/mfa/verify` | `{docs, read}` | `docs:read` |

Failures, three different answers:

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

The router mints tokens at `POST /test/token`, so no gate logs in through a part it is
not judging.

## Storage

Two collections in `records:store`.

`docs`, indexed on `tag`:

```json
{ "title": "Deploying to the lattice", "text": "…", "tag": "ops" }
```

`stepups`, indexed on `subject` — the mark one part writes and another reads:

```json
{ "subject": "ada", "verified_at": 1787232000, "secret": "…" }
```

A document's id is the one `records:store` minted; `create` returns an `Entry` whose
`id` is a fresh ULID, so nothing here generates one.

## Config

Read with `wasi:config/store`. Both have defaults, and both are set low by the gates:

| key | default | meaning |
|---|---|---|
| `answer-budget` | `50` | answers a subject may spend per period |
| `answer-period-secs` | `86400` | the period that budget covers |
| `answer-cache-ttl-secs` | `3600` | how long an answer stays fresh |
| `stepup-ttl-secs` | `900` | how long a verified step-up lasts |

## Part 1 — `library` (`src/library.rs`)

### `POST /api/docs` — `docs:write`

Body `{"title": string, "text": string, "tag": string}`, all required and non-empty;
otherwise `400 {"error":"invalid_doc"}`.

Stores the document **and** indexes it, and both must happen or the library lies:

```rust
use crate::bindings::search::index::index as search;
search::index_doc(id: &str, text: &str, tags: &[String]) -> Result<(), SearchError>
```

The indexed text is `title` + `"\n"` + `text` — a question naming the title has to find
it — and the tags are exactly `[tag]`. `201 {"id": "<doc id>"}`.

### `GET /api/docs/{id}` — `docs:read`

`200` with the stored document (its `id` included), `404 {"error":"not_found"}`.

### `GET /api/search?q=&tag=&limit=` — `docs:read`

```rust
search::query(query: &str, mode: search::Mode, tags: &[String], limit: u32)
    -> Result<Vec<Hit>, SearchError>
```

`Mode::Any`, `limit` default 5 capped at 20, `tag` optional and passed as the tag
filter when present. Answer `200 {"hits":[{"id","score","title"}]}` — **the title comes
from the store**, because a hit carries only an id and a score and a caller cannot use
a list of ULIDs. Ordered by descending score. A query matching nothing is
`200 {"hits":[]}`, never a 404: an empty library and a bad question are the same shape
to a caller and neither is an error.

## Part 2 — `answer` (`src/answer.rs`)

### `POST /api/answer` — `docs:read` **and** a stepped-up session

Body `{"question": string}`, non-empty or `400 {"error":"invalid_question"}`.

The order of checks is the specification, because each one exists to stop the cost of
the next:

1. **Step-up.** Read `stepups` for `principal.subject` (an index lookup on `subject`;
   `find_by` wants the value JSON-encoded — `"\"ada\""`, not `ada` — and a wrong query
   returns `Ok(vec![])`, which reads exactly like "never verified"). Absent, or
   `verified_at` older than `stepup-ttl-secs`, is
   `403 {"error":"step_up_required"}`. Nothing else runs.
2. **Cache.** The key is `answer:` + the question, verbatim.
   ```rust
   use crate::bindings::cache::store::cache;
   cache::get(key: &str) -> Result<Option<Vec<u8>>, CacheError>
   cache::set(key: &str, value: &[u8], ttl_seconds: u64) -> Result<(), CacheError>
   ```
   A hit is answered immediately as `200` with `"cached": true`, **no quota spent and
   no model call**. This is the property the whole part exists for.
3. **Retrieval.** `search::query` with `Mode::Any` and `limit` 3. **No hits is
   `404 {"error":"no_sources"}`, and no budget has been touched at that point** — asking a model a question the library cannot
   support is how this app invents things.
4. **Budget.** Only now — after retrieval found something, so a question the library
   cannot support costs the caller nothing:
   ```rust
   use crate::bindings::quota::meter::meter;
   meter::reserve(subject: &str, amount: u64, limit: u64, period_seconds: u64)
       -> Result<Balance, QuotaError>
   pub struct Balance { pub used: u64, pub limit: u64, pub remaining: u64, pub resets_at: u64 }
   ```
   `amount` is 1, `limit` is `answer-budget`, `period_seconds` is
   `answer-period-secs`.

   **`Exceeded`'s payload is not a duration.** It carries the units still available —
   which is always `0` when a request for one unit is refused. So the refusal is
   `429 {"error":"budget_exhausted","retry_after":<secs>}` with the seconds taken from
   the balance instead: `meter::peek(...)` returns `resets_at` as unix seconds, and
   `retry_after` is `resets_at - now`. A part that passes the variant's payload through
   as a duration answers `retry_after: 0`, which tells a caller to retry immediately,
   forever, and nothing anywhere reports an error.

   That refusal is **not cached**. And `reserve` is the reservation: do not also
   `record_usage` for the same answer, or every question costs two.
5. **The model**, with the retrieved text as its context:
   ```rust
   use crate::bindings::ai::inference::inference as ai;
   ai::generate(prompt: &str, context: &str) -> Result<String, AssistError>
   ```
   `prompt` is the question. `context` is the hits' documents joined by `"\n\n"`, each
   as `title` + `"\n"` + `text`, read from the store. An `AssistError` is
   `503 {"error":"answer_unavailable"}`, and nothing is cached.

Success is `200`:

```json
{ "answer": "…", "sources": ["<doc id>", "…"], "cached": false, "remaining": 1 }
```

`sources` are the hit ids in order, `remaining` is the balance's `remaining`, and
`cached` is `false` here and `true` on a cache hit. On a cache hit `remaining` is the
balance `meter::peek` reports — reading a balance is free, spending it is not.

## Part 3 — `stepup` (`src/stepup.rs`)

The second factor, and the mark the `answer` part reads.

### `POST /api/mfa/enroll` — `docs:read`

```rust
use crate::bindings::otp::totp::authenticator as totp;
totp::provision(issuer: &str, account: &str) -> Result<Provisioned, OtpError>
pub struct Provisioned { pub secret: String, pub uri: String }
```

Issuer `"docsearch"`, account `principal.subject`. Store the secret in `stepups` for
that subject with `verified_at` **0** — enrolled is not verified — and answer
`201 {"secret": "...", "uri": "..."}`. Enrolling again replaces the secret and resets
`verified_at` to 0: a caller who re-enrolls has a new authenticator and has not used
it yet.

### `POST /api/mfa/verify` — `docs:read`

Body `{"code": string}`. Verify against the stored secret:

```rust
totp::verify(secret: &str, code: &str, period: u32, digits: u8, skew: u32)
    -> Result<bool, OtpError>
```

`period` 30, `digits` 6, `skew` 1 — one window either side, because a code typed at the
boundary of a 30-second window is a correct code and refusing it is a bug users cannot
distinguish from a broken app.

* `Ok(true)` → set `verified_at` to now, answer `200 {"verified": true}`
* `Ok(false)` → `401 {"error":"bad_code"}`, and `verified_at` is **left alone**: a wrong
  code must not log a verified session out, and must not verify it either
* not enrolled → `409 {"error":"not_enrolled"}`
* `Err(_)` → `503 {"error":"totp_unavailable"}`

### `GET /api/mfa` — `docs:read`

`200 {"enrolled": bool, "verified": bool}`, where `verified` accounts for
`stepup-ttl-secs`. This is the part's own read of the same state `answer` checks, and
the two must agree — the join gate is where that is established.

## The router, which no part may write

`src/lib.rs` dispatches and owns:

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` |
| `POST /test/token` | `{"subject","scopes"}` → `201 {"token"}` |
| `POST /test/seed` | indexes and stores three documents, returns their ids — so `answer` and `stepup` can be judged before `library` exists |
| `POST /test/stepup` | `{"subject"}` → writes a verified `stepups` mark directly, so `answer` can be judged before `stepup` exists |
| `GET /test/doc/{id}` | the stored document, raw |

It also gives every part `crate::cfg_u64(key, default)` — the four config keys above as
numbers. Reading config is plumbing; what a part does with the number is the goal.

## How you are judged

By real HTTP requests against the running component, and by what the compiled
component **imports**. A hand-rolled TOTP, an in-memory answer cache, a counter in the
store instead of the meter, or a fuzzy title match instead of the index all answer
correctly on the happy path and all fail their gate, because each gate reads which
capabilities the artifact actually calls.
