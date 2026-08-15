# jco-helpdesk — the helpdesk SaaS domain as ONE wasm HTTP component

Rung 1 of [docs/apps/HELPDESK.md](../../HELPDESK.md): the core ticket loop of a
multi-tenant support SaaS, built as the **`helpdesk:app`** Rust component and
composed with every capability it needs into a single self-contained `.wasm`
that exports `wasi:http/incoming-handler`.

```
helpdesk_domain.composed.wasm
  = helpdesk-domain (Rust HTTP handler)    exports wasi:http/incoming-handler
  + auth-guard.composed                    auth:identity (accounts/session/rbac) + rate-limit + audit
  + record-store                           records:store (tickets/messages)
  + fsm-workflow                           fsm:workflow (the ticket lifecycle)
  + id-generate                            id:generate  (HD-XXXXXX public refs)
  + markdown                               md:render    (safe Markdown -> HTML replies)
```

Built with `just compose-helpdesk`. The only remaining imports are generic
WASI (`keyvalue`, `config`, `clocks`, `random`, `http`) — bound by the host.

## The point: the lifecycle is data, not code

`new → open → pending → solved → closed` (reopen on requester reply) is a
declarative `fsm:workflow` definition. Replies FIRE events; the engine
rejects illegal moves (close from open → 409) and keeps an append-only
history that doubles as the ticket's audit trail. Internal agent notes are
invisible to requesters and move the machine nowhere.

## Run

```bash
# from comp/: build + compose the app wasm
just compose-helpdesk       # -> components/target/helpdesk_domain.composed.wasm
cp components/target/helpdesk_domain.composed.wasm examples/jco-helpdesk/

cd examples/jco-helpdesk
npm install
npm test                    # serves the wasm via WASI HTTPServer, drives it over HTTP
npm start                   # serve on :3007 for manual curl
```

## Frontend + native host

`public/index.html` is a single-file SPA (vanilla JS, no build step): login /
register, ticket list, thread view with server-rendered Markdown, agent verbs
driven by the FSM's `allowed_events`, internal notes. Serve it and the API
from ONE process on the native Rust host, persisted to NATS JetStream KV:

```bash
docker compose -f infra/compose.yaml up -d nats
just host-helpdesk          # wasmtime host + SPA + NATS KV on 0.0.0.0:3007
```

Benchmarks of this exact setup (NATS vs memory KV): [bench/HELPDESK-BENCH.md](../../bench/HELPDESK-BENCH.md).

## What the test proves (all over real HTTP, no Node domain code)

- requester/agent/admin roles via the composed auth-guard (register/login
  audit events appear on stderr — that's audit-log, composed in for free)
- requester isolation: other requesters get 404, not 403 (no existence leak)
- internal notes are agent-only and hidden from requesters
- agent reply drives `new→open→pending`; requester reply `pending→open`;
  reply on `solved` reopens; `closed` is terminal and rejects messages
- lifecycle verbs (`triage|solve|close|reopen`) are agent-only and FSM-legal
- Markdown bodies render to sanitized HTML (`**export**` → `<strong>`)
- the FSM history endpoint replays the entire journey in order

## Routes

```
POST /auth/register {email,password,role?}    role: requester|agent|admin
POST /auth/login    GET /auth/me    POST /auth/logout
POST /api/tickets                    {subject, body, priority?}
GET  /api/tickets                    agents: all; requesters: own
GET  /api/tickets/{id}               + messages + allowed_events (agents)
POST /api/tickets/{id}/messages      {body, internal?}
POST /api/tickets/{id}/state         {event}    agent-only
POST /api/tickets/{id}/assign        {subject}  agent-only
GET  /api/tickets/{id}/history       the FSM audit trail
```

## Scope

Rung 1 only: no event-bus fan-out, notifications, SLA timers, search,
quotas, or billing yet — those are rungs 3–6 in docs/apps/HELPDESK.md and compose in
the same way (`wac plug` more capabilities, zero host changes).
