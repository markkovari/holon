# invoice:copilot — the contract

An invoice copilot where the model writes the words and the money component does the
arithmetic. One component, three parts, written at the same time by three agents.

This document is the specification. It is not writable: if something here is wrong or
missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and the other
parts answer between generations.

## The shape of the whole

```
POST /api/invoices ──────────────────▶ invoices ─▶ authorize ─▶ throttle ─▶ draft invoice
POST /api/invoices/{id}/lines/suggest ▶ copilot  ─▶ model names the lines,
                                                    money:amount divides the total
POST /api/invoices/{id}/post ────────▶ posting  ─▶ idempotency ─▶ ledger balances ─▶ posted
```

**The model is trusted with words and with nothing else.** It is a fine way to name three
lines of an invoice and a terrible way to divide 100.00 by three — it answers 33.33 three
times, loses a cent, and is confident about it. Every number in this app comes from
`money:amount`.

## Identity

Every `/api/*` route needs `Authorization: Bearer <token>`, resolved with one call:

```rust
use crate::bindings::auth::identity::authorizer as authz;
authz::authorize(token: &str, required: &Permission) -> Result<Principal, AuthError>
```

| route | permission | scope |
|---|---|---|
| `POST /api/invoices` | `{invoices, write}` | `invoices:write` |
| `GET /api/invoices/{id}`, `GET /api/invoices/{id}/entry` | `{invoices, read}` | `invoices:read` |
| `POST /api/invoices/{id}/lines/suggest` | `{invoices, write}` | `invoices:write` |
| `POST /api/invoices/{id}/post` | `{invoices, post}` | `invoices:post` |

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

## Money, and how it is stored

**Every amount is an integer in minor units** — 10000 is €100.00 — and every one of them
comes out of `money:amount`:

```rust
use crate::bindings::money::amount::arithmetic as money;

money::parse(decimal: &str, currency: &str)  -> Result<Amount, MoneyError>
money::add(a: &Amount, b: &Amount)           -> Result<Amount, MoneyError>
money::allocate(total: &Amount, shares: u32) -> Result<Vec<Amount>, MoneyError>
money::format(a: &Amount)                    -> Result<String, MoneyError>
pub struct Amount { pub units: i64, pub currency: String }
```

A decimal string is **never** parsed by hand: `"1.005"`, `"1,00"`, `"€1.00"` and `"1e2"`
each have an answer and it is not the obvious one, and a copilot that rounds its own cents
is a copilot that quietly overcharges.

**`parse` wants exactly the currency's number of decimal places**, and reports anything else
as `UnknownCurrency` — so in EUR, `"100.00"` parses and `"100"`, `"100.0"` and `"100.000"`
are all refused, each with an error naming the CURRENCY rather than the format. It is the
one thing about this interface that reads as a different problem than it is: a caller
debugging "unknown currency EUR" will look everywhere except at the number of digits.
`MoneyError::UnknownCurrency` / `CurrencyMismatch` are `400 {"error":"bad_money"}`.

## Storage

One collection, `invoices`, indexed on `state` and `customer`:

```json
{
  "customer": "acme-gmbh",
  "currency": "EUR",
  "state": "draft",
  "created_at": "2026-08-20T09:00:00Z",
  "lines": [
    { "memo": "Discovery workshop, day one", "units": 3334 },
    { "memo": "Discovery workshop, day two", "units": 3333 },
    { "memo": "Written summary and next steps", "units": 3333 }
  ],
  "total_units": 10000,
  "entry": {
    "id": "<ledger entry id>",
    "posted_at": "2026-08-20T09:02:00Z",
    "lines": [ { "account": "…", "amount": 10000, "side": "debit" }, … ]
  }
}
```

`state` is `"draft"` until posted, then `"posted"`. `lines` is `[]` on a new invoice and
`entry` is absent until posting. An invoice's id is the one `records:store` minted.

## Config

| key | default | meaning |
|---|---|---|
| `max-attempts` | `5` | invoices per subject per window (read by `rate-limiter`) |
| `lockout-window` | `300` | that window, in seconds (read by `rate-limiter`) |
| `idempotency-ttl-secs` | `86400` | how long a posting key is remembered |
| `revenue-account` | `revenue:services` | the credit account for a posted invoice |
| `receivable-account` | `assets:receivable` | the debit account |

## Part 1 — `invoices` (`src/invoices.rs`)

### `POST /api/invoices` — `invoices:write`

Body `{"customer": string, "currency": string}`, both required and non-empty; otherwise
`400 {"error":"invalid_invoice"}`. The currency must be one `money:amount` accepts — check
it by parsing a zero written with the right number of decimals for it, and answer
`400 {"error":"bad_money"}` when that fails: a
currency nobody can add up is an invoice that cannot be totalled, and finding that out at
posting time is finding it out too late. Stored with `state` `"draft"`, `lines` `[]`,
`total_units` `0`, `created_at` RFC3339 UTC. `201 {"id": "<invoice id>"}`.

**The throttle takes two calls.** `ratelimit:guard` is a fixed-window *failure counter*, not
a throughput meter: `check` asks whether the key is locked and counts nothing;
`record_failure` is what counts.

```rust
use crate::bindings::ratelimit::guard::limiter as rl;
rl::check(key: &str)          -> Result<u32, LimitError>  // remaining, or Locked(secs)
rl::record_failure(key: &str) -> Result<(), LimitError>
```

`check` first — `Err(Locked(secs))` is `429 {"error":"rate_limited","retry_after":secs}` and
nothing is stored — then `record_failure` once the invoice is accepted. The key is
`principal.subject`. `Err(BackendUnavailable(_))` is
`503 {"error":"rate_limit_unavailable"}`.

### `GET /api/invoices/{id}` — `invoices:read`

`200` with the stored invoice (its `id` included), `404 {"error":"not_found"}`.

## Part 2 — `copilot` (`src/copilot.rs`)

### `POST /api/invoices/{id}/lines/suggest` — `invoices:write`

The one route that costs a model call, and the one place the split between words and numbers
is enforced.

Body `{"prose": string, "total": string, "shares": <2..12>}`. `prose` describes the work,
`total` is a decimal string in the invoice's currency, `shares` is how many lines to split
it into. A missing or out-of-range field is `400 {"error":"invalid_suggestion"}`.

1. The invoice must exist and be `draft`. Unknown is `404 {"error":"not_found"}`, already
   posted is `409 {"error":"already_posted"}` — and no model call.
2. `money::parse(total, invoice.currency)` — a bad decimal is `400 {"error":"bad_money"}`
   before anything is spent.
3. **The model names the lines, and only names them:**
   ```rust
   use crate::bindings::ai::inference::inference as ai;
   ai::extract(text: &str, fields: &[String]) -> Result<Vec<(String, String)>, AssistError>
   ai::generate(prompt: &str, context: &str) -> Result<String, AssistError>
   ```
   Either verb is fine; what matters is that what comes back is used as **text**. Ask for
   `shares` short line descriptions of the work in `prose`. Take the first `shares` of them;
   if fewer come back, pad with `"Line <n>"` — a model that returns two descriptions for
   three shares must not become an invoice with two lines, because the total would then not
   be the total. An `AssistError` is `503 {"error":"suggest_unavailable"}` and the invoice is
   untouched.
4. **`money::allocate(total, shares)` produces the numbers.** Not the model, and not
   division written here: 100.00 into 3 is `[3334, 3333, 3333]` and every other answer is
   either short a cent or over by one. Pair the allocated amounts with the descriptions in
   order.
5. Store `lines` and `total_units` = the sum via `money::add` over the allocated amounts —
   which must equal the parsed total, and if it does not, the allocation was not used.

Success is `200`:

```json
{ "lines": [{"memo": "…", "units": 3334}, …], "total_units": 10000, "total": "100.00" }
```

`total` is `money::format` of the parsed total. Two suggestions on one invoice replace the
lines; that is not an error, it is a draft.

## Part 3 — `posting` (`src/posting.rs`)

### `POST /api/invoices/{id}/post` — `invoices:post`

Posting is the only irreversible thing this app does, so it happens **once**.

The caller sends `Idempotency-Key: <string>`. Missing or empty is
`400 {"error":"idempotency_key_required"}` — a posting route without one is a
double-charge waiting for a retry, and retries are not optional in anything that talks to a
payment system.

```rust
use crate::bindings::idempotency::guard::store as idem;
idem::begin(key: &str, ttl_seconds: u64) -> Result<Option<CachedResponse>, IdemError>
idem::complete(key: &str, status: u16, body: &[u8]) -> Result<(), IdemError>
pub struct CachedResponse { pub status: u16, pub body: Vec<u8> }
```

1. `begin` with the key and `idempotency-ttl-secs`.
   * `Ok(Some(cached))` → **answer the cached status and body verbatim.** Not a fresh
     posting, not a 409: the caller is retrying and must get the same answer it would have
     got the first time.
   * `Err(InProgress)` → `409 {"error":"in_progress"}`. Another request holds this key.
   * `Err(BackendUnavailable(_))` → `503 {"error":"idempotency_unavailable"}`. **Never carry
     on:** an idempotency guard that is down is not permission to post twice.
2. The invoice must exist, be `draft`, and have lines. Unknown is `404`, already posted is
   `409 {"error":"already_posted"}`, no lines is `409 {"error":"nothing_to_post"}`.
3. **Build the double entry and let the ledger refuse it:**
   ```rust
   use crate::bindings::ledger::doubleentry::ledger;
   ledger::validate(e: &Entry) -> Result<(), LedgerError>
   pub struct Entry { pub id: String, pub memo: String, pub lines: Vec<Line> }
   pub struct Line { pub account: String, pub amount: i64, pub side: Side }
   pub enum Side { Debit, Credit }
   ```
   One debit of `total_units` to `receivable-account`, one credit of `total_units` to
   `revenue-account`, memo the invoice id. `Err(Unbalanced((d, c)))` is
   `500 {"error":"unbalanced","debits":d,"credits":c}` — it means this part built the entry
   wrongly, and posting it anyway is how a ledger stops being one.
4. Store `entry` on the invoice, set `state` `"posted"`, and **`complete` the key with the
   status and body you are about to answer** — so the retry in step 1 can return it. A
   posting that succeeds without `complete` is a posting that will happen again.

Success is `201 {"entry": "<invoice id>", "total_units": <n>, "posted_at": "…"}`.

### `GET /api/invoices/{id}/entry` — `invoices:read`

`200` with the stored entry, `404 {"error":"not_posted"}` when there is none.

## The router, which no part may write

`src/lib.rs` dispatches and owns:

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` |
| `POST /test/token` | `{"subject","scopes"}` → `201 {"token"}` |
| `POST /test/seed` | one draft invoice, and one with three lines already allocated — so `copilot` and `posting` can be judged before `invoices` exists |
| `GET /test/invoice/{id}` | the stored invoice, raw |

It also gives every part `crate::cfg(key, default)`.

## How you are judged

By real HTTP requests against the running component, and by what the compiled component
**imports**. The arithmetic is checked to the cent: three lines that sum to 99.99 instead of
100.00 fail, and so does a second posting with the same key. Dividing by hand, rounding
cents, trusting the model's numbers, or posting without the guard each answer plausibly on
the one path a developer tries — and each fails its gate.
