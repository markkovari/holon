# moderation:queue — the contract

Content goes in, a decision comes out, and the model does not have the last word. One
component, three parts, written at the same time by three agents.

This document is the specification. It is not writable: if something here is wrong or
missing, write `CONTRACT-REQUEST.md` (first line the subject, the rest why) and the other
parts answer between generations.

## The shape of the whole

```
POST /api/items ─────────────▶ intake  ──▶ authorize ─▶ throttle ─▶ queued as pending
POST /api/rules ─────────────▶ queue   ──▶ policy:guard.set-rules
POST /api/items/{id}/review ─▶ verdict ──▶ model says … ─▶ POLICY DECIDES ─▶ published
```

**The precedence is the specification.** The model produces a label; the policy either
overrules it or stays silent. A decision that reports only its outcome cannot be audited,
so every decision records what the model said *and* what the policy did to it.

## Identity

Every `/api/*` route needs `Authorization: Bearer <token>`, resolved with one call:

```rust
use crate::bindings::auth::identity::authorizer as authz;
authz::authorize(token: &str, required: &Permission) -> Result<Principal, AuthError>
```

A scope `"<target>:<action>"` grants `Permission { target, action }`.

| route | permission | scope |
|---|---|---|
| `POST /api/items` | `{items, write}` | `items:write` |
| `GET /api/items/{id}`, `GET /api/queue`, `GET /api/events` | `{items, read}` | `items:read` |
| `POST /api/items/{id}/review` | `{items, moderate}` | `items:moderate` |
| `POST /api/rules`, `GET /api/rules` | `{items, moderate}` | `items:moderate` |

| condition | status | body |
|---|---|---|
| header absent or empty | 401 | `{"error":"unauthenticated"}` |
| `invalid-token`, `expired`, `malformed` | 401 | `{"error":"unauthenticated"}` |
| `insufficient-scope` | 403 | `{"error":"forbidden"}` |
| `backend-unavailable`, `internal` | 503 | `{"error":"auth_unavailable"}` |

## Storage

One collection, `items`, indexed on `state` and `author`:

```json
{
  "text": "buy followers at spam.example",
  "author": "ada",
  "state": "pending",
  "submitted_at": "2026-08-20T09:00:00Z",
  "decision": {
    "final": "blocked",
    "model_said": "review",
    "model_confidence": 620,
    "policy_rule": "no-links",
    "policy_reason": "…",
    "decided_at": "2026-08-20T09:01:00Z"
  }
}
```

`state` is `"pending"` until reviewed, then the decision's `final`: one of `"allowed"`,
`"flagged"`, `"blocked"`. `decision` is absent while pending. An item's id is the one
`records:store` minted.

## Config

| key | default | meaning |
|---|---|---|
| `max-attempts` | `5` | submissions per subject per window (read by `rate-limiter`) |
| `lockout-window` | `300` | that window, in seconds (read by `rate-limiter`) |
| `policy-domain` | `moderation` | the `policy:guard` domain rules are stored under |

## Part 1 — `intake` (`src/intake.rs`)

### `POST /api/items` — `items:write`

Body `{"text": string}`, non-empty or `400 {"error":"invalid_item"}`. Stored with
`author` = `principal.subject`, `state` = `"pending"`, `submitted_at` RFC3339 UTC.
`201 {"id": "<item id>"}`.

**The throttle takes two calls, and this is the part everybody gets wrong.**
`ratelimit:guard` is a fixed-window *failure counter*, not a throughput meter: it counts
what you tell it to count, and `check` alone counts nothing.

```rust
use crate::bindings::ratelimit::guard::limiter as rl;
rl::check(key: &str)          -> Result<u32, LimitError>  // remaining, or Locked(secs)
rl::record_failure(key: &str) -> Result<(), LimitError>
```

`check` first — `Err(Locked(secs))` is
`429 {"error":"rate_limited","retry_after":secs}` and nothing is stored — then
`record_failure` once the item is accepted. The key is `principal.subject`, and the gate
submits as two different subjects to see whether it was keyed on something else.
`Err(BackendUnavailable(_))` from either call is `503 {"error":"rate_limit_unavailable"}`:
a limiter that is down must not silently become no limiter.

### `GET /api/items/{id}` — `items:read`

`200` with the stored item (its `id` included), `404 {"error":"not_found"}`.

## Part 2 — `verdict` (`src/verdict.rs`)

### `POST /api/items/{id}/review` — `items:moderate`

The one route in this app that costs a model call, and the one place precedence is
decided. In order:

1. The item must exist and be `pending`. Unknown is `404 {"error":"not_found"}`; already
   decided is `409 {"error":"already_decided","final":"<the stored final>"}` — and no
   model call.
2. **The model's opinion**, over the item's stored text:
   ```rust
   use crate::bindings::ai::inference::inference as ai;
   ai::classify(text: &str, labels: &[String]) -> Result<LabelScore, AssistError>
   pub struct LabelScore { pub label: String, pub confidence: u32 }  // 0..=1000 MILLI-units
   ```
   The labels are exactly `["allow", "flag", "block"]`. A label outside that set is
   `502 {"error":"unexpected_label"}`. An `AssistError` is
   `503 {"error":"model_unavailable"}` and the item stays `pending` — no half-written
   decision.
3. **The policy, which decides.** Not advice, and not a second opinion:
   ```rust
   use crate::bindings::policy::guard::guard as policy;
   policy::can(domain: &str, action: &str, principal: &[Attr], target_attrs: &[Attr])
       -> Result<Decision, PolicyError>
   pub struct Attr { pub key: String, pub value: String }
   pub struct Decision { pub allowed: bool, pub rule_id: String, pub reason: String }
   ```
   `domain` is the `policy-domain` config value, `action` is `"publish"`. `principal`
   carries `subject` = the item's author. `target_attrs` carries the facts a rule can be
   written against, and these three exactly:

   | key | value |
   |---|---|
   | `model_label` | the model's label — `allow`, `flag` or `block` |
   | `has_link` | `"true"` or `"false"`: does the item's text contain `://` |
   | `author` | the item's author |

   **A rule references an attribute as `resource.<key>` or `principal.<key>`.** A bare
   `has_link` in a condition is not a reference — the engine treats an operand without one
   of those two prefixes as a LITERAL STRING, so `{left: "has_link", op: eq, right:
   "true"}` compares the text `"has_link"` with `"true"`, is false forever, and never
   matches anything. No error is raised at write time or at evaluation time; the rule
   simply never fires. So the attributes above are written by `verdict` under those keys
   and referenced by a rule as `resource.model_label`, `resource.has_link`,
   `resource.author`, and the principal's as `principal.subject`.

   **Precedence, and the trap inside it:** the engine's default is DENY, so when nothing
   matches it answers `allowed: false` with an **empty `rule_id`** and the reason
   `"no matching rule (default deny)"`. A part that reads `allowed` on its own therefore
   blocks everything, passes every test written with a matching rule, and silently makes
   the model's opinion irrelevant in the one case it was supposed to be decisive.

   So `rule_id` is what you branch on. A decision whose `rule_id` is non-empty is a rule
   that matched, and it decides — `allowed: false` means `final` is `"blocked"` whatever
   the model said. An **empty `rule_id` means no rule matched**, and then the model's
   label decides:
   `allow` → `"allowed"`, `flag` → `"flagged"`, `block` → `"blocked"`. A
   `PolicyError` is `503 {"error":"policy_unavailable"}` — a policy engine that is down
   must never be read as "no rule matched", because that is the model winning by default.
4. **Publish it**, so something downstream can act:
   ```rust
   use crate::bindings::event::bus::bus;
   bus::publish(topic: &str, payload: &[u8]) -> Result<String, BusError>
   ```
   Topic `"moderation.decided"`, payload the JSON `{"item":"<id>","final":"<final>"}`.
   A publish failure is `503 {"error":"bus_unavailable"}` **after** the item was written:
   the decision stands, and the event is what failed. Say so rather than pretending the
   review did not happen.

Success is `200` with the decision object exactly as it is stored (see Storage), and the
item's `state` set to `final`.

## Part 3 — `queue` (`src/queue.rs`)

### `POST /api/rules` — `items:moderate`

Body `{"rules":[{"id","action","effect","priority","conditions":[{"left","op","right"}]}]}`,
handed to `policy:guard` under the `policy-domain`:

```rust
policy::set_rules(domain: &str, rules: &[Rule]) -> Result<(), PolicyError>
pub struct Rule { pub id: String, pub action: String, pub effect: Effect,
                  pub conditions: Vec<Condition>, pub priority: u32 }
pub enum Effect { Allow, Deny }
pub enum Op { Eq, Ne, InList, Lt, Gt, Has }
```

`effect` is `"allow"` or `"deny"`; `op` is one of `eq`, `ne`, `in-list`, `lt`, `gt`,
`has`. An unknown effect or op is `400 {"error":"invalid_rule"}` — and it must be caught
here, because a rule the engine rejects later is a rule nobody wrote down. `204` on
success.

### `GET /api/rules` — `items:moderate`

`200 {"rules":[…]}` as `policy:guard` holds them, over `get_rules`. Answering from
something this part stored separately is how the rules a reviewer reads stop being the
rules the reviewer's decisions used.

### `GET /api/queue?state=&limit=` — `items:read`

`200 {"items":[{"id", …item…}]}`, oldest first (a queue, not a stack). `state` defaults
to `"pending"` and is an **index lookup** — `find_by` wants the value JSON-encoded
(`"\"pending\""`, not `pending`), and a wrong query returns `Ok(vec![])`, which reads
exactly like an empty queue. `limit` defaults to 20, capped at 100.

### `GET /api/events?topic=&max=` — `items:read`

What has been published, over the bus, so a reviewer can see decisions leaving the
system:

```rust
bus::poll(topic: &str, group: &str, max: u32) -> Result<Vec<Event>, BusError>
pub struct Event { pub id: String, pub topic: String, pub payload: Vec<u8>, pub at: u64 }
```

`topic` defaults to `"moderation.decided"`, `group` is `"queue-reader"`, `max` defaults to
20. Answer `200 {"events":[{"id","topic","at","payload":<the JSON, parsed>}]}`. Do **not**
`ack`: a read that consumes is not a read, and the gate polls twice.

## The router, which no part may write

`src/lib.rs` dispatches and owns:

| route | what it does |
|---|---|
| `GET /health` | `200 {"ok":true}` |
| `POST /test/token` | `{"subject","scopes"}` → `201 {"token"}` |
| `POST /test/seed` | stores two pending items and returns their ids — so `verdict` and `queue` can be judged before `intake` exists |
| `POST /test/rules` | writes a fixed deny-links rule straight through `policy:guard` — so `verdict` can be judged before `queue` exists |
| `GET /test/item/{id}` | the stored item, raw |
| `GET /test/events` | what is on the bus, read with a fixture consumer group — so a publish is observable while the part owning `/api/events` is still a stub |

It also gives every part `crate::cfg(key, default)` for config strings.

## How you are judged

By real HTTP requests against the running component, and by what the compiled component
**imports**. An `if text.contains("://")` in place of a policy engine, a counter in the
store instead of the limiter, and a decision written but never published each answer
correctly on the one path a developer tries by hand — and each fails its gate, because
the gate reads which capabilities the artifact actually calls.
