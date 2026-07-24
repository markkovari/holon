# track — a Linear-lite project tracker (the complex composition)

A **project tracker**: projects, issues on a board (backlog → todo → in progress
→ done), comments, labels, per-project membership, full-text search, a live
activity feed, and an AI thread-summary — with a real **Vite + TypeScript SPA
baked into the wasm**. Chosen as the *complex* showcase: where the others each
prove **one** axis, track stitches **five** into one composed component over
**~15 capability contracts** and no bespoke business crate. It's the proof that
the thesis scales past a demo — a plausible SaaS backend *is* composition.

Same shape as the others — one **`track-domain`** HTTP component that exports
`wasi:http` and imports only WIT contracts — but far wider: auth+RBAC, a state
machine, search, an event bus, outbound webhooks, an LLM boundary, ABAC, and an
embedded SPA, all linked at compose time with `wac`.

![The track board: an admin creates a project and files issues, a member moves them across backlog → todo → in progress → done, comments render as markdown, an AI button summarizes the thread, full-text search narrows the board, and a live activity feed streams every change over SSE — all from one composed wasm component with the SPA baked in](docs/media/track.gif)

## Five axes in one app

| axis | what you see | contracts |
|---|---|---|
| **write** | create projects / issues / comments; markdown bodies render to safe HTML | `records:store`, `md:render` |
| **auth + RBAC** | register / login / session; an admin creates projects; a **member** may write a project's issues, a **non-member is 403** (per-project ABAC) | `auth:identity` (accounts/authorizer/session/rbac), `policy:guard` |
| **read** | full-text issue search; opaque-cursor issue lists | `search:index`, `paginate:cursor` |
| **stream** | every mutation publishes to a bus; the activity feed streams live over **SSE** | `event:bus` |
| **background** | `POST /api/tick` sweeps **stale `in_progress`** issues and flags them — a timer-driven workload, not a request | `event:bus` (+ the clock) |
| **out** | an issue transition fires a **webhook:sign-signed** outbound webhook | `notify:dispatch`, `webhook:sign` |
| **AI** | summarize an issue's whole comment thread; mock LLM by default, real via provider swap | `ai:inference` (over `llm:inference`) |

Plus the **UI axis**: the SPA is a Vite + TS build embedded in its own
`ui:assets` component (`track-assets`) and served by `track-domain` — the wasm
is self-contained, no `--static-dir`.

The stream axis end-to-end, two boards side by side: the left files and moves
issues, and the right — a **separate** board instance holding its own SSE
connection — sees each change land in its activity feed and its board reload
**live**, proving the `event:bus` fan-out (one component, no WebSocket):

![Two track boards side by side: Alice (left) files two issues and moves one across the board; Bob (right), a separate session, watches issue.created and issue.moved events stream into his activity feed live over SSE and his board update in lockstep — the event-bus fan-out across independent clients](docs/media/track-sse.gif)

## Why it's still (almost) pure composition

The `track-domain` crate is ~600 lines of *glue*: parse a request, introspect
the token, decide access, call a contract, publish an event, shape JSON. There
is **no** hand-rolled auth, TF-IDF, state machine, HMAC, pub/sub, or LLM client
— each is a contract:

- **auth** — `accounts::register`/`login`, `authorizer::introspect` on every
  guarded route; a global `admin` role (via `rbac::assign-role`) gates project
  creation.
- **membership (ABAC)** — writing a project's issues is a `policy:guard`
  decision: a rule `principal.role ∈ {member, lead}` seeded at boot, enforced
  per request against the caller's membership row. A non-member gets a 403 the
  domain code never spells out.
- **lifecycle** — the issue board is an `fsm:workflow` machine
  (`backlog→todo→in_progress→done`, with `reopen`/`stop`/`shelve`); illegal
  moves are the engine's 409, and the transition log is the history.
- **search / feed / webhook / AI** — `search:index`, `event:bus`,
  `webhook:sign`+`notify:dispatch`, `ai:inference` — each one call.

## The provider-swap (AI axis)

`track-domain` imports `ai:inference/inference`; `ai-inference` in turn imports
`llm:inference/inference`. That boundary is the swap point:

- `just compose-track` plugs the **mock** LLM (`llm-inference`) — deterministic,
  offline, what the e2e and demo use.
- Swap the plug for `openai-provider` (`compose-ai-openai`) and the summary is
  real — `track-domain` is unchanged. The domain never names a vendor.

## Product surface (one component)

```
POST /auth/register {email,password,role?}          (open; first user → admin)
POST /auth/login    {email,password}                (open) → token
GET  /auth/me
POST /api/projects  {key,name}                       (admin)
GET  /api/projects
POST /api/projects/{pk}/members {subject,role}       (admin or project lead)
POST /api/issues    {project,title,body,label?}      (project member)
GET  /api/issues    ?project=&status=&limit=&after=
GET  /api/issues/{id}
POST /api/issues/{id}/move  {event}                  (member; fsm transition)
POST /api/issues/{id}/comments {body}                (member)
POST /api/issues/{id}/summarize                      (AI over the thread)
GET  /api/search    ?q=&project=
POST /api/tick                                       (background stale sweep)
GET  /api/stream    ?after=seq                       (SSE activity feed)
GET  /  /assets/*  /<spa-route>                      (the baked SPA)
```

## Domain model (`records:store`)

- **project** — `{key, name, lead, counter}`; `counter` mints per-project issue
  numbers (`ENG-1`, `ENG-2`, …). Indexed by `key`.
- **member** — `{key: "project:subject", project, subject, role}` — the ABAC
  input; indexed by `key` + `project`.
- **issue** — `{ref, project, title, body, label, assignee, reporter, status,
  flagged}`; indexed by `project` + `status`. `status` mirrors the fsm state;
  `flagged` is set by the stale sweep. Title+body are indexed into
  `search:index`, faceted `project:…` / `label:…`.
- **comment** — `{issue, author, body}`; indexed by `issue`. Rendered with
  `md:render` on read.

## Component map

**Reused as-is (15):** `auth:identity` (the composed auth-guard), `policy:guard`,
`records:store`, `fsm:workflow`, `search:index`, `paginate:cursor`, `event:bus`,
`notify:dispatch`, `webhook:sign`, `ai:inference` (+ `llm:inference` mock),
`md:render`, and `ui:assets` (the baked SPA). Plus host WASI (keyvalue, clocks,
config, http). This is the widest single-component composition in the repo.

**New (2):** `track-domain` (`track:app`, exports `wasi:http`) — the glue; and
`track-assets` (`ui:assets`) — the Vite+TS SPA embedded in its own component.

**The compose chain** (biggest in the repo — 13 plugs): auth-guard is
pre-composed (`+ rate-limiter + audit-log`), ai-inference is pre-composed
(`+ mock llm`), then `track-domain` is plugged with those two plus records, fsm,
search, event-bus, notify, webhook-sign, policy, paginate, markdown, and
track-assets → one self-contained `track_domain.composed.wasm`.

## Build order (each rung is demoable)

1. **Auth + projects + issues** — register/login, admin creates a project, a
   member files an issue. `just e2e-track` proves a non-admin can't create a
   project and a non-member can't write issues (403).
2. **Lifecycle + comments + search** — `fsm:workflow` moves across the board
   (illegal move → 409); comments render markdown; `search:index` finds an issue
   by text. e2e asserts each.
3. **Stream + background + out** — every mutation publishes to `event:bus`; the
   SSE feed streams it; `POST /api/tick` sweeps stale issues; a move fires a
   signed webhook. e2e asserts an `issue.created` frame reaches the SSE feed.
4. **AI + baked SPA** — `ai:inference` summarizes the thread (mock LLM); the
   Vite+TS SPA is baked into `track-assets` and served by the domain. `just
   host-track`, then open the board.
5. **Bench** — the composition-cost dimension: per-request instantiation of a
   15-import component, and the auth+ABAC guard overhead per protected call. See
   `bench/TRACK-BENCH.md`.

## Non-goals (v1)

Real-time drag-and-drop persistence (the board moves are button clicks →
`move`), attachments, multi-workspace tenancy beyond the single `track` tenant,
notifications beyond the one webhook, and OAuth/OIDC login (local accounts
only — the contract supports OIDC, the demo doesn't wire it). The showcase
proves the **breadth of composition**, not a Linear feature-parity clone.
