# support:desk — the contract

A support desk whose replies are drafted by a model and delivered at least once. One
component, three parts, written at the same time by three agents.

This document is the specification. It is not writable: if something here is wrong or
missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and the other
parts answer between generations.

## The shape of the whole

```
POST /api/tickets ───────────▶ tickets ──▶ authorize ─▶ stored open
POST /api/tickets/{id}/reply ▶ reply   ──▶ CSRF ─▶ budget ─▶ model drafts ─▶ ENQUEUED
POST /api/deliver ───────────▶ courier ──▶ claim ─▶ send ─▶ ack, or fail and retry
```

**Nothing is sent inline.** A drafted reply goes into the outbox and the delivery pass
takes it from there. That is the difference between an app that loses a reply when the far
end is down and one that does not, and it is invisible in any request that succeeds.

## Identity, and the CSRF token

Every `/api/*` route needs `Authorization: Bearer <token>`, resolved with one call:

```rust
use crate::bindings::auth::identity::authorizer as authz;
authz::authorize(token: &str, required: &Permission) -> Result<Principal, AuthError>
```

A scope `"<target>:<action>"` grants `Permission { target, action }`.

| route | permission | scope |
|---|---|---|
| `POST /api/tickets` | `{tickets, write}` | `tickets:write` |
| `GET /api/tickets/{id}`, `GET /api/tickets` | `{tickets, read}` | `tickets:read` |
| `POST /api/tickets/{id}/reply` | `{tickets, reply}` | `tickets:reply` |
| `POST /api/deliver`, `GET /api/dead-letters`, `POST /api/dead-letters/{id}/replay` | `{tickets, deliver}` | `tickets:deliver` |

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

**`POST /api/tickets/{id}/reply` additionally needs a session and its CSRF token.** A desk
is a browser app, and a POST nobody checked came from the page is the oldest hole there is.
The agent's session id arrives in the `x-session` header and the token in `x-csrf`:

```rust
use crate::bindings::session::store::store as sessions;
sessions::verify_csrf(id: &str, token: &str) -> Result<(), SessionError>
```

| condition | status | body |
|---|---|---|
| either header missing | 403 | `{"error":"csrf_required"}` |
| `Err(CsrfMismatch)` | 403 | `{"error":"csrf_invalid"}` |
| `Err(NotFound)` | 403 | `{"error":"session_expired"}` |
| `Err(BackendUnavailable(_))` | 503 | `{"error":"session_unavailable"}` |

The router opens a session at `POST /test/session` and returns both its id and its token,
so no gate has to log in through a part it is not judging.

## Storage

One collection, `tickets`, indexed on `state` and `customer`:

```json
{
  "subject": "Invoice does not match my plan",
  "body": "…what the customer wrote…",
  "customer": "webhook:https://example.test/hooks/ada",
  "state": "open",
  "opened_at": "2026-08-20T09:00:00Z",
  "reply": {
    "text": "…what the model drafted…",
    "event": "<outbox event id>",
    "drafted_at": "2026-08-20T09:01:00Z"
  }
}
```

`state` is `"open"` until a reply is drafted, then `"answered"`. `reply` is absent until
then. `customer` is the delivery address, `"webhook:<url>"` — the desk speaks webhooks, and
the prefix is there so a later channel does not need a new field. A ticket's id is the one
`records:store` minted.

## Config

| key | default | meaning |
|---|---|---|
| `reply-budget` | `50` | drafted replies a tenant may spend per period |
| `reply-period-secs` | `86400` | that period |
| `max-attempts` | `8` | delivery attempts before dead-lettering (read by `outbox`) |
| `base-backoff` | `5` | backoff base seconds, doubled per attempt (read by `outbox`) |

## Part 1 — `tickets` (`src/tickets.rs`)

### `POST /api/tickets` — `tickets:write`

Body `{"subject": string, "body": string, "customer": string}`, all required and non-empty,
and `customer` must start with `"webhook:"` — otherwise
`400 {"error":"invalid_ticket"}`. A delivery address nothing can deliver to is a ticket
that will dead-letter later for a reason nobody can act on, so it is refused here.
Stored with `state` `"open"` and `opened_at` RFC3339 UTC. `201 {"id": "<ticket id>"}`.

### `GET /api/tickets/{id}` — `tickets:read`

`200` with the stored ticket (its `id` included), `404 {"error":"not_found"}`.

### `GET /api/tickets?state=&limit=` — `tickets:read`

`200 {"tickets":[{"id", …ticket…}]}`, oldest first. `state` defaults to `"open"` and is an
index lookup — `find_by` wants the value JSON-encoded (`"\"open\""`, not `open`), and a
wrong query returns `Ok(vec![])`, which reads exactly like a desk with nothing waiting.
`limit` defaults to 20, capped at 100.

## Part 2 — `reply` (`src/reply.rs`)

### `POST /api/tickets/{id}/reply` — `tickets:reply`, plus CSRF

The one route that costs a model call. In order, and each check exists to stop the cost of
the next:

1. **CSRF**, per the table above. Nothing else runs first: a request that did not come from
   the page is not a request.
2. The ticket must exist and be `open`. Unknown is `404 {"error":"not_found"}`; already
   answered is `409 {"error":"already_answered"}` — and no model call.
3. **Budget**:
   ```rust
   use crate::bindings::quota::meter::meter;
   meter::reserve(subject: &str, amount: u64, limit: u64, period_seconds: u64)
       -> Result<Balance, QuotaError>
   pub struct Balance { pub used: u64, pub limit: u64, pub remaining: u64, pub resets_at: u64 }
   ```
   `subject` is `principal.tenant`, `amount` 1, from the `reply-budget` and
   `reply-period-secs` config. `Exceeded`'s payload is the units still available — **not a
   duration** — so `retry_after` comes from `meter::peek`'s `resets_at` minus now:
   `429 {"error":"budget_exhausted","retry_after":<secs>}`.
4. **The draft**:
   ```rust
   use crate::bindings::ai::inference::inference as ai;
   ai::generate(prompt: &str, context: &str) -> Result<String, AssistError>
   ```
   `prompt` is the ticket's `subject`, `context` its `body`. An `AssistError` is
   `503 {"error":"draft_unavailable"}` and the ticket stays `open`.
5. **Enqueue it — do not send it.**
   ```rust
   use crate::bindings::outbox::dispatch::queue as outbox;
   outbox::enqueue(topic: &str, payload: &[u8], delay_seconds: u64)
       -> Result<String, OutboxError>
   ```
   Topic `"support.reply"`, `delay_seconds` 0, payload the JSON
   `{"ticket":"<id>","target":"<the customer field>","subject":"Re: <ticket subject>","body":"<the draft>"}`.
   An enqueue failure is `503 {"error":"outbox_unavailable"}` and the ticket stays `open`
   with no reply: a draft nothing will deliver is worse than no draft, because the budget
   was already spent on it and nobody will ever know it existed.

   **This part must not send.** Sending inline is the failure this whole app is about, and
   the gate checks it the only way that works: it runs a webhook receiver, grants the
   component egress to it, and requires that nothing arrives while this part is judged. (Not
   an import check — all three parts compile into one component, so the artifact's imports
   say what the COMPONENT calls and never which part called it.)

Then the ticket is updated — `state` `"answered"`, `reply` per Storage, `event` the id
`enqueue` returned — and the answer is `202 {"event":"<outbox event id>","remaining":<n>}`.
**202, not 200:** nothing has been delivered yet, and saying 200 to a customer's agent is
how a desk claims to have replied when it has only decided to.

## Part 3 — `courier` (`src/courier.rs`)

### `POST /api/deliver?max=` — `tickets:deliver`

One delivery pass. `max` defaults to 10, capped at 50.

```rust
outbox::claim(max: u32, lease_seconds: u64) -> Result<Vec<Event>, OutboxError>
outbox::ack(id: &str) -> Result<(), OutboxError>
outbox::fail(id: &str) -> Result<State, OutboxError>
pub struct Event { pub id: String, pub topic: String, pub payload: Vec<u8>,
                   pub state: State, pub attempts: u32, pub created: u64, pub not_before: u64 }
```

Claim with a lease of 30 seconds, then for each event send it:

```rust
use crate::bindings::notify::dispatch::dispatcher as notify;
notify::send(msg: &notify::Message) -> Result<u16, notify::NotifyError>
pub struct Message { pub channel: Channel, pub target: String,
                     pub subject: String, pub body: String }
```

`channel` is `Channel::Webhook`, and `target` is the payload's `target` with the
`"webhook:"` prefix removed. The subject and body come from the payload.

**What happens on failure is the whole part:**

* `Ok(status)` → `ack`. `send` answers `Ok` only for a 2xx; the status is there to be
  recorded, not to be re-checked.
* `Err(DeliveryFailed(reason))` → `fail`. The far end refused — `reason` carries its status.
* `Err(_)` (unreachable, unsupported channel, backend down) → `fail`.

**`Err` does not say whether the failure was transient**, and a courier must not guess: a
refusal and an unreachable host arrive the same way, and treating one as permanent is how a
reply is dropped for an outage that lasted a minute. Every `Err` is a `fail`, and the outbox
decides what happens next.

**What `fail` RETURNS is the part nobody reads.** It answers the event's new state:
`pending` means it will be retried after backoff, `dead` means it has exhausted
`max-attempts` and nothing will ever retry it again. A courier that ignores that return
value never knows a reply was abandoned — it is not an error, nothing is logged, and the
customer simply never hears back. Count the dead ones and report them.

Never `ack` an event you did not deliver, and never `fail` one you did: the first loses
replies, the second sends them twice.

Answer `200`:

```json
{ "claimed": 3, "delivered": 2, "failed": 1, "dead": 0 }
```

### `GET /api/dead-letters?max=` — `tickets:deliver`

`200 {"events":[{"id","topic","attempts","payload":<the JSON, parsed>}]}` over
`outbox::dead_letters`. `max` defaults 20, capped 100.

### `POST /api/dead-letters/{id}/replay` — `tickets:deliver`

`outbox::replay(id)` puts it back to pending. `204` on success,
`404 {"error":"not_found"}` if the outbox does not know it. A dead letter that cannot be
replayed is a support reply that is simply gone, so this route is not optional.

## The router, which no part may write

`src/lib.rs` dispatches and owns:

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` |
| `POST /test/token` | `{"subject","tenant","scopes"}` → `201 {"token"}` |
| `POST /test/session` | opens a session → `201 {"session","csrf"}` |
| `POST /test/seed` | stores two open tickets aimed at a `target` you pass, and returns their ids — so `reply` and `courier` can be judged before `tickets` exists |
| `POST /test/enqueue` | puts one reply straight into the outbox for a `target` you pass → `201 {"event"}` — so `courier` can be judged before `reply` exists |
| `GET /test/ticket/{id}` | the stored ticket, raw |

It also gives every part `crate::cfg_u64(key, default)`.

## How you are judged

By real HTTP requests against the running component, and by what the compiled component
**imports**. The gates run a sink they can break on purpose: a reply that is sent inline, a
non-2xx that is acked anyway, or a delivery that is retried after it already arrived each
answer correctly on the one path a developer tries by hand, and each fails its gate.
