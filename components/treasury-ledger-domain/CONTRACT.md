# treasury:ledger — the contract

Accounts, transfers between them, and an independent reconciliation. One component, three
parts, written at the same time by three agents.

This document is the specification. It is not writable: if something here is wrong or
missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and the other
parts answer between generations.

**Read this first, because it is the difference between this goal and an ordinary one.** Every
route here is easy to write correctly for one request at a time, and the gates do not send one
request at a time. They send twenty-four at once, and they check afterwards that the money adds
up. A part that passes every single-request test and loses one credit in twenty-four is the
failure this app is about, and it is the normal outcome of the obvious implementation.

## The shape of the whole

```
POST /api/accounts/{id}/credit ─▶ accounts   ─▶ read, add, write — under contention
POST /api/transfers ───────────▶ transfers   ─▶ lock the pair, move both sides, journal it
POST /api/reconcile ───────────▶ reconcile   ─▶ recompute every balance from the journal
```

`accounts` and `transfers` move money. `reconcile` is the only part that may be believed about
it: it recomputes each balance from the journal and reports the drift, so the join gate does
not have to take the word of the code that did the moving.

## Identity

Every `/api/*` route needs `Authorization: Bearer <token>`, resolved with one call:

```rust
use crate::bindings::auth::identity::authorizer as authz;
authz::authorize(token: &str, required: &Permission) -> Result<Principal, AuthError>
```

| route | permission | scope |
|---|---|---|
| `POST /api/accounts`, `POST /api/accounts/{id}/credit` | `{accounts, write}` | `accounts:write` |
| `GET /api/accounts/{id}` | `{accounts, read}` | `accounts:read` |
| `POST /api/transfers` | `{transfers, write}` | `transfers:write` |
| `GET /api/transfers/{id}`, `GET /api/journal`, `POST /api/reconcile` | `{transfers, read}` | `transfers:read` |

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

## Money

**Every amount is an integer of minor units** and every one of them comes from
`money:amount`. `parse` wants exactly the currency's decimal places and calls anything else an
unknown currency: in EUR, `"1.00"` parses and `"1"` does not.

```rust
use crate::bindings::money::amount::arithmetic as money;
money::parse(decimal: &str, currency: &str)   -> Result<Amount, MoneyError>
money::add(a: &Amount, b: &Amount)            -> Result<Amount, MoneyError>
money::subtract(a: &Amount, b: &Amount)       -> Result<Amount, MoneyError>
money::compare(a: &Amount, b: &Amount)        -> Result<i8, MoneyError>
pub struct Amount { pub units: i64, pub currency: String }
```

## Storage

`accounts`, indexed on `name`:

```json
{ "name": "left", "currency": "EUR", "units": 10000, "opened_at": "2026-08-21T09:00:00Z" }
```

`transfers`, indexed on `state`:

```json
{
  "from": "<account id>", "to": "<account id>", "units": 2500, "currency": "EUR",
  "state": "settled", "key": "<idempotency key>", "created_at": "…",
  "journal": { "id": "<transfer id>", "lines": [ … ] }
}
```

`state` is one of `pending`, `settled`, `refused`, `compensated`. A record's id is the one
`records:store` minted.

## The one thing this whole app is about

`records:store` is optimistic. `update` takes the revision the caller believes the record is
at, and refuses when that is no longer true:

```rust
records::get(collection, id)                          -> Result<Entry, StoreError>
records::update(collection, id, data, expected_revision) -> Result<Entry, StoreError>
StoreError::RevisionConflict(u64)   // the revision it is ACTUALLY at, now
```

Two requests that read the same revision and both write will see one of them refused. The
obvious implementation — read, add, write once — therefore **drops that write**, and the caller
is told `409` for a credit that should simply have happened. The measured behaviour, on this
host, of twenty-four concurrent credits of 1.00 against one account written that way: the
account ends at **23.00**, and once at 21.00. Nothing errors, nothing is logged, and the
balance is simply wrong.

A conflict is not a failure. It is the store saying *"read again"*. Every writer in this app
retries on `RevisionConflict` — re-read, recompute from what is there NOW, write again — with a
bounded number of attempts, and answers `503 {"error":"contended"}` only if it truly cannot get
through. **`409` is never a correct answer to a credit.**

## Part 1 — `accounts` (`src/accounts.rs`)

### `POST /api/accounts` — `accounts:write`

Body `{"name": string, "currency": string, "start": string}` — `start` a decimal in that
currency, defaulting to the currency's zero. `400 {"error":"invalid_account"}` for a missing
name or currency, `400 {"error":"bad_money"}` if `money` refuses the currency or the amount.
`201 {"id": "<account id>"}`.

### `POST /api/accounts/{id}/credit` — `accounts:write`

Body `{"amount": string}`, a positive decimal in the account's currency. A zero or negative
amount is `400 {"error":"invalid_amount"}` — a credit that removes money is a transfer written
by somebody who did not want to think about the other side.

`200 {"units": <new balance>}`, and **every concurrent credit must land**: the gate fires
twenty-four at once and requires the balance to be exactly the sum. Unknown account is `404`.

### `GET /api/accounts/{id}` — `accounts:read`

`200` with the stored account and its `id`. `404 {"error":"not_found"}`.

## Part 2 — `transfers` (`src/transfers.rs`)

### `POST /api/transfers` — `transfers:write`, and an `Idempotency-Key`

Body `{"from": "<id>", "to": "<id>", "amount": "<decimal>"}`. Missing key is
`400 {"error":"idempotency_key_required"}`; `from == to` is `400 {"error":"same_account"}`;
either account unknown is `404 {"error":"not_found"}`; different currencies is
`400 {"error":"currency_mismatch"}`.

This is the part where a first attempt is wrong, and the interesting thing is that the obvious
fix is also wrong.

**1. A lease is not mutual exclusion, and reaching for one is the first wrong answer.** This
repository has `lock:mutex`, and it is not in this app's world on purpose. `acquire` is a
load-then-store — it reads the current lease, sees nothing live, and writes its own
(`components/lock-mutex/src/lib.rs:157`) — so two callers arriving together both find the pair
free and both take it. Measured, with the lease as the only guard and everything else correct:
**2 of 12** transfers of an entire balance settled, which is one more than the money allows.
The `fence` on a lease exists because of this; the lease alone is a courtesy to reduce churn,
never a guarantee.

**2. The debit is the serialisation point, and the store's revision is what makes it one.**
Read the source account, compare, and write with `expected_revision` set to the revision you
just read:

```rust
let (entry, doc) = read(from);           // revision r
if balance(doc) < moving { refuse }      // decided against revision r
records::update("accounts", from, debited, entry.revision)   // committed only if still r
```

Of two callers who both saw enough money, exactly one commits: the other gets
`RevisionConflict`, goes round again, re-reads, and this time the comparison refuses it. **The
check and the write have to be the same CAS.** A check followed by an unconditional write is
the double-spend this app is built to catch, and it passes every sequential test.

That is also why the two sides are not symmetric. The **debit** may legitimately fail — there
may not be enough — so it is a conditional write that can refuse. The **credit** cannot fail on
its merits, so it is a retry loop that goes round until it lands. Once the debit has committed,
the money is out of the source and has to arrive somewhere: if the credit cannot be written at
all, put it back on the source before answering, because that is the only path in this app that
can destroy any.

The gate proves it the only way that works: twelve concurrent transfers of the entire balance,
of which **exactly one** may succeed. The other eleven are
`409 {"error":"insufficient_funds"}`, no account may go negative, and the sum across the two
accounts must be what it was before.

**3. A refusal is a state, not a silence.** Record the transfer either way — `settled` or
`refused` — and drive it through the lifecycle rather than assigning strings:

```rust
use crate::bindings::fsm::workflow::engine as fsm;
fsm::define(name: &str, def: &Definition)             -> Result<(), FsmError>
fsm::create_instance(machine: &str, instance: &str)   -> Result<Status, FsmError>
fsm::fire(machine: &str, instance: &str, event: &str) -> Result<Status, FsmError>
pub struct Definition { pub states: Vec<String>, pub initial: String,
                        pub transitions: Vec<Transition>, pub terminal: Vec<String> }
pub struct Transition { pub event: String, pub source: String, pub target: String }
```

Machine name `"transfer"`, states `pending → settled | refused`, and `settled → compensated`.
Defining it more than once is not an error; assuming it has already been defined is —
`create_instance` on an undefined machine is `UnknownMachine`, and nothing else in the app will
have defined it for you.

**4. The journal is what `reconcile` reads, and it must balance.** On a settled transfer, write
one `journal` document with two lines — a debit of `units` on `from`, a credit of `units` on
`to` — and let the ledger refuse it if it does not balance:

```rust
use crate::bindings::ledger::doubleentry::ledger;
ledger::validate(e: &Entry) -> Result<(), LedgerError>
```

Store it in the `journal` collection as
`{ "transfer": "<transfer id>", "from": "<id>", "to": "<id>", "units": <n>, "at": "…" }`. A
journal write that fails after the balances moved is `500 {"error":"journal_lost"}` — say it
rather than hiding it; `reconcile` is what finds it otherwise, and that is far too late to be
the first anyone hears of it. **Only a settled transfer is journalled**: a refusal moved nothing.

**Idempotency.** `begin` the key before anything, `complete` it with the answer:
`Ok(Some(cached))` is answered verbatim, `Err(InProgress)` is `409 {"error":"in_progress"}`,
and a guard that is unavailable is `503` and never permission to move money twice.

Success is `201 {"transfer": "<id>", "from_units": <n>, "to_units": <n>}`.

### `GET /api/transfers/{id}` — `transfers:read`

`200` with the stored transfer, `404 {"error":"not_found"}`.

## Part 3 — `reconcile` (`src/reconcile.rs`)

The auditor. It may not trust the balances; it recomputes them.

### `POST /api/reconcile` — `transfers:read`, and an `Idempotency-Key`

Body `{"opened": [{"account": "<id>", "units": <n>}, …]}` — what each account is *believed* to
have started with, which is the only thing reconciliation cannot derive. For every account in
that list:

1. sum the journal: every line where it is `to` adds, every line where it is `from` subtracts,
   all through `money::add` / `money::subtract`;
2. add the opening figure;
3. compare with the stored balance via `money::compare`.

Answer `200`:

```json
{
  "checked": 2,
  "drift": [ { "account": "<id>", "expected": 7500, "actual": 7400, "delta": -100 } ],
  "balanced": false,
  "journal_lines": 13
}
```

`drift` lists only accounts that disagree, `balanced` is whether it is empty. **A reconciliation
that reports `balanced: true` without reading the journal is the worst possible outcome of this
app**, so the gate seeds a journal that disagrees with the balances on purpose and requires the
exact delta.

Same idempotency key twice returns the same answer verbatim — a reconciliation is a report, and
running it twice must not produce two different truths.

### `GET /api/journal?limit=` — `transfers:read`

`200 {"lines":[…]}`, oldest first, `limit` default 50 capped 500.

## The router, which no part may write

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` |
| `POST /test/token` | `{"subject","scopes"}` → `201 {"token"}` |
| `POST /test/seed` | two accounts, `left` and `right`, each starting at `start` → `201 {"account_ids", "units"}` |
| `POST /test/journal` | writes one journal line straight through, so `reconcile` can be judged before `transfers` exists |
| `GET /test/account/{id}` | the stored account, raw, with its revision |

It also gives every part `crate::cfg(key, default)`.

## How you are judged

By concurrent HTTP requests, and by arithmetic afterwards. Twenty-four credits at once must all
land; twelve simultaneous transfers of the whole balance must produce exactly one settlement and
no negative balance; the sum across accounts must not change; and an independent recomputation
from the journal must agree with every stored balance to the minor unit. Each gate also reads
which capabilities the compiled component actually calls: a lock written here, arithmetic done
by hand, or a lifecycle kept in strings will each answer one request beautifully and fail.
