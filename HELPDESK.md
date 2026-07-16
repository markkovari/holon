# helpdesk — a mid-sized SaaS over composed capability contracts

A multi-tenant support/ticketing product (Zendesk-lite). Chosen because every
feature maps onto an existing contract in the catalog — the app is almost pure
composition, and the few gaps are honest new components, not glue.

Pattern mirrors `vet-domain`: one **`helpdesk-domain`** component that exports
`wasi:http` and imports only WIT contracts; every capability behind it is a
swappable reference implementation.

## Product surface

Three actor types, one HTTP component:

| Actor | Surface | Auth |
|---|---|---|
| **End user** (requester) | submit ticket via portal or email, view own tickets, reply, CSAT rating | session or signed magic-link |
| **Agent** | queue views, reply (public/internal note), assign, change state, macros, search | session + OTP (2FA), RBAC role `agent`/`admin` |
| **Tenant admin** | team mgmt, SLA policies, billing, API keys, outbound webhooks, branding, CSV export | RBAC role `admin` |
| **Machine** | REST API + inbound webhooks (e.g. "create ticket from alert") | signed API key + rate limit + quota |

## Domain model (all rows in `records:store`, keys prefixed `t:{tenant}:`)

- **tenant** — plan, locale, branding, settings
- **user** — requester or agent; agents have RBAC roles
- **ticket** — subject, requester, assignee, priority, tags, state, SLA deadlines
- **message** — per-ticket thread; kind = public | internal | system; markdown body; attachment refs
- **sla-policy** — first-response / resolution targets per priority
- **macro** — canned reply template
- **api-key**, **webhook-endpoint**, **invoice-line** (usage events)

## Ticket lifecycle = `fsm:workflow`

```
new → open → pending (waiting on requester) → solved → closed
        ↑______________________________________|   (reopen on reply)
```

Guards on transitions: only agents may move to `solved`; `closed` is terminal
after N days (scheduler fires the close). Every transition emits an event on
`event:bus` — that single stream fans out to webhooks, notifications, audit,
and metering. **The event bus is the spine; nothing calls a side-effect
directly.**

## The flows and what they compose

### 1. Ticket create (portal or API)
`rate-limiter` → `auth-guard`/api-key verify → `validate` (payload schema) →
`pii-redact` (strip card numbers etc. before persist) → `id-generate` +
`slug` (public ref like `HD-4821`) → `records:store` → `fsm-workflow` init →
`quota` meter (tickets/month = billable unit) → `event-bus` publish →
`outbox` (guaranteed side-effects).

### 2. Email in → ticket   *(gap: needs new `mail:parse` component)*
Inbound MIME hits `webhook-ingest` (provider POST, e.g. SES/Mailgun shape) →
`mail:parse` extracts sender/subject/text/attachments → match `In-Reply-To`
to existing ticket or create new → attachments through `upload-policy` →
`blob-store`.

### 3. Agent reply
session (`session-store`) → `policy-guard` (ABAC: agent belongs to tenant,
ticket not closed) → `markdown` render → store message → FSM `open→pending` →
`event-bus`.

### 4. Notification fan-out (event-bus consumer)
event → `notify-dispatch` with body from `email-render` template, localized
via `i18n-catalog` per user locale. `idempotency-guard` keyed on event-id so
redelivery never double-sends.

### 5. Outbound webhooks (tenant integrations)
event → filter by tenant's endpoint subscriptions → `webhook-sign` (HMAC) →
`outbox` for retry/backoff → deliver via `notify-dispatch` http. This is
`webhook-relay` reused nearly verbatim.

### 6. SLA engine
On create/transition compute deadlines from sla-policy → `scheduler-timer`
arms `first-response-due` / `resolution-due` timers → on fire, check state;
if breached: escalate priority, notify, emit `sla.breached`. Reply before
deadline cancels the timer.

### 7. AI assist (agent-side, flag-gated)
`feature-flags` per tenant → draft reply: thread → `llm:inference`
(deterministic mock in dev, `openai-provider` in prod, key from
`secrets-vault`) → also auto-tag + sentiment on create. Never auto-sends;
drafts only.

### 8. Search
On message/ticket write → `search-index` upsert (tenant-prefixed index).
Agent queue views = search queries + `pagination`.

### 9. Billing (reuses `billing-ledger` wholesale)
`quota` meters usage (tickets created, AI calls, seats) → monthly rollup job
(`scheduler-timer`) → invoice lines via `money` arithmetic → `csv` export →
`billing-ledger` app is the admin-facing ledger UI.

### 10. Ops
Public uptime = `status-page` app pointed at helpdesk's health endpoints.
Every privileged mutation → `audit-log`. Hot reads (queue counts, tenant
settings) → `cache` + `cache-backing`. Portal UI shell = `static-assets`.
Runtime config = `config-store`, secrets = `secrets-vault`, distributed
one-at-a-time jobs (rollup) = `lock-mutex`.

## Component map

**Reused as-is (28):** auth-guard, session-store, otp, policy-guard,
rate-limiter, quota, idempotency-guard, records-store, blob-store,
upload-policy, cache, cache-backing, config-store, secrets-vault, event-bus,
outbox, webhook-ingest, webhook-sign, notify-dispatch, email-render,
i18n-catalog, fsm-workflow, scheduler-timer, search-index, audit-log,
feature-flags, lock-mutex, static-assets.

**Pure compute reused (10):** id-generate, slug, validate, markdown, money,
csv, pagination, pii-redact, jsonpatch (ticket PATCH endpoint), geo
(requester locale hint).

**AI (3):** llm-inference (dev), openai-provider (prod), ai-inference.

**Existing apps reused as sub-apps (3):** billing-ledger (invoice ledger),
status-page (public status), webhook-relay pattern (flow 5).

**New components needed (4):**

| new | contract | why nothing covers it |
|---|---|---|
| `helpdesk-domain` | `helpdesk:app` exports wasi:http | the app itself |
| `mail-parse` | `mail:parse@0.1.0` — MIME → {from, subject, text, html, attachments, in-reply-to} | pure compute; nothing parses email |
| `csat` | tiny: signed one-click rating link → score on ticket | could fold into domain; separate only if reused |
| `assignment` | round-robin / load-based agent routing, pure fn `(agents, workloads) → agent` | pure compute, trivially testable |

Tenancy is a key-prefix convention (`t:{tenant}:`) enforced in the domain
component, not a new contract — same choice link-shortener already made.

## Build order (each rung is demoable)

1. **Core loop** — create/reply/list tickets, FSM, sessions. (domain + ~10 existing comps)
   ✅ done: `components/helpdesk-domain` + `just compose-helpdesk` + `examples/jco-helpdesk` (8 e2e tests)
2. **Multi-tenant + RBAC + API keys** — policy-guard, quota, rate-limiter, audit.
3. **Events out** — event-bus spine, notifications, outbound signed webhooks, i18n emails.
4. **Email in** — `mail-parse` (first new component), webhook-ingest wiring.
5. **SLA + search** — scheduler-timer, search-index, assignment.
6. **Money** — quota rollup → billing-ledger, CSV export.
7. **AI + polish** — flags, LLM drafts, CSAT, status-page, geo/i18n defaults.

## Non-goals (v1)

Live chat/WebSockets, knowledge base/KB articles, per-tenant custom fields
(jsonpatch gets you 80%), SSO/SAML, mobile. All addable later without new
contracts except chat.
