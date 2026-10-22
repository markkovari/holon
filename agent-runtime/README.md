# agent-runtime

The autonomous half of a Holon agent. An agent is a **spec** (name,
description, capabilities, triggers, model) plus a **brain** (this crate's
bounded tool-using loop, memory and run log) plus an optional lattice **front
door** (`gateway/`, one generic component deployed once per agent).

```
cron / event / HTTP ──► runtime ──► model (local fm · OpenAI-compatible · Anthropic)
                          │  ▲
              tools ◄─────┘  └── approvals (a human, for sensitive calls)
   remember · recall · now · http_get · read/write_file · emit_event · agent:<other>
```

## Run it

Embedded in the GPUI console (`gpui-console`), or headless:

```sh
cargo run --release -- --state-dir ~/.holon-agents \
    [--listen 127.0.0.1:18017] [--local-url http://127.0.0.1:PORT]   # fm serve, mlx_lm, ...
```

State lives under `--state-dir`: `agents/<name>.json` (the spec — edit it and
the next run uses it, nothing rebuilds), `memory/`, `runs/` (every step of
every run), `workspaces/<name>/` (the only place its file tools can reach).

## An agent

```json
{ "name": "build-watcher",
  "description": "You watch CI and tell people when it breaks.",
  "capabilities": [
    {"name": "http_get"},
    {"name": "summarize", "description": "write a 3-line summary"},
    {"name": "fetch", "wit": "os:http/client.get", "description": "get a page"},
    {"name": "agent:notifier"} ],
  "allow_hosts": ["ci.example.com"],
  "triggers": [ {"kind": "schedule", "cron": "*/10 * * * *", "prompt": "check the latest build"},
                {"kind": "event", "topic": "deploy"} ],
  "model": {"kind": "local"} }
```

* **Capabilities** are text for the model, with an optional `wit` ref for the
  platform. A built-in tool name (or `agent:<other>`) makes it callable; any
  other name is an ability the model is told about. A tool the agent was not
  granted is refused, never dispatched.
* **Triggers**: HTTP (always, through the gateway), `schedule` (5-field cron
  UTC, `@hourly`, `@every 30s`; add `"notify": true` and a chat bridge posts
  each result to you, otherwise it is only in the run log; add `"project": "rowing"`
  and the run is for that project: the agent must be a member, the task says so, and
  the bridge posts the result in that project's room), `event` (`emit_event` from another agent, or
  `POST /events/<topic>`; the payload is the task).
* **Photos**: a run can come with photos from the agent's inbox: `POST /agents/<n>/run?attach=a.jpg,b.png`
  (at most four, names only, so they cannot leave that agent's inbox). They go to the agent's model with
  the first message, a large one shrunk and an iPhone HEIC converted (macOS `sips`). A model sees them only
  if its spec says `"vision": true` (`deepseek-chat` does; the on-device and small local models do not): any
  other is told a photo was attached and asks for it in words instead of failing. A photo that is missing
  or is not an image is said in the message, not fatal. Every step of a run sends the pictures again, so
  they are billed again; keep a photo-reading agent's `max_steps` small.
* **API keys** live in the platform's vault (`secrets-vault`, ADR-0051), and the console's Keys tab
  is the easy way in: add a key there and switch it on for an agent. By hand, a key is a vault secret
  whose value is `{"host": "api.github.com", "key": "ghp_..."}` (optional `"header"`, default
  `Authorization`, and `"scheme"`, default `Bearer`; `""` sends the bare key), and the runtime reads it
  with a fetch token minted for just those refs, from `HOLON_PLATFORM_URL`, `HOLON_SECRETS_ORG` and
  `HOLON_FETCH_TOKEN`, else from `~/.comp-secrets/vault.json` (what the console writes). The agent
  needs `"api_keys": ["github"]` and `http_get`. The secret, not the spec, decides where a key may go:
  `http_get` sends it only to that host (reachable then without `allow_hosts`), and the model reads
  `[api key]` in its place. An agent can give another agent only keys it holds itself.
* **One runtime, many faces.** Element (through `agent-matrix`) and the board (`holon-console`,
  `agents-board --runtime http://127.0.0.1:18017`) drive the same daemon over `/v1` (below). Its
  `/v1/keys` manage API keys in the vault of the platform `--platform-file` names. `infra/launchd/install.sh` runs it all: nats, the platform, the runtime, the bridge.
* **Limits**: `max_steps`, `max_tokens` per run, `daily_token_budget`.
  A run always ends with an answer; an over-budget one says why.
* **Approval**: `http_get` and `write_file` ask a human first unless listed in
  `auto_approve`. A scheduled run with no human waits `approval_timeout`, then
  is denied; the denial goes back to the model as the tool's error.

## Agents waking and working with each other

| how | what | durable | traced |
|---|---|---|---|
| **call** `agent:<name>` | synchronous; answer and token cost come back | — | child span |
| **task** `spawn_task` / `task_result` | same, without waiting | in memory | child span |
| **event** `emit_event` | fan-out on a topic | yes: log + per-agent offset, in order | child span |
| **store** `store_put` | a shared key/value blackboard; a write that CHANGES a value wakes `store_change` watchers | yes | child span |
| **timer** `schedule_self` | one-shot wake-up of yourself | yes (`timers.json`) | new trace |

* **Events are durable.** A paused agent, or a restart, loses nothing: each
  subscriber has an offset per topic and acks after a run. A new subscriber
  hears the future, not the topic's whole history. `filter` on an event trigger
  wakes the agent only for matching payloads.
* **The store is how agents should usually cooperate.** `store_put` reports
  `changed: true/false`, so "tell me when X changes" is a `store_change`
  trigger, not a procedure a small model must follow. Namespaces are granted
  per spec (`store.read` / `store.write`); `private` is always an agent's own.
  The prompt names the stores and topics an agent was granted, because a model
  cannot guess them (and falls back on `private`, which nobody else sees).
* **Waking is granted, not assumed.** `emit_event` only reaches `topics_out`
  (globs like `deploy.*`); `spawn_task` only targets `agent:<name>` capabilities.
* **Guards** (refused before the model is called, and logged as `Dropped` runs):
  a chain deeper than 6 wake-ups, an agent woken twice by the same topic in one
  chain (a cycle), `max_runs_per_min`, and a circuit breaker after repeated
  failures. A chain also shares one token budget: a woken run can spend no more
  than what its caller had left.
* **`must_call`** lists tools every run must call before its plain-text reply
  counts. A reply that comes too soon gets a reminder (twice); a run that still
  never calls them fails visibly. Small models otherwise write an intermediate
  note as prose and end their own task.

## Coding sessions

An agent granted `session_start` can hand a piece of coding to a [`comp-agentd`](../reconciler/daemons/agentd/)
session ([ADR-0119](../docs/adr/0119-an-agent-can-start-a-coding-session.md)): an agent that reads, writes
and runs commands in a workspace, each step asked about first. The agent chooses the task; the spec
chooses everything else.

```json
{ "name": "lead",
  "capabilities": [{"name": "session_start"}, {"name": "session_result"}, {"name": "session_close"}],
  "sessions": { "workspaces": ["/Users/me/work/holon"], "budget_usd": 2.0,
                "auto_approve": ["list_dir", "read_file"], "max_concurrent": 2 } }
```

* `session_start {"task": "...", "workspace": "own"}` returns a session id at once. `workspace` is `own`
  (the agent's own directory) or one of `sessions.workspaces`, by path or last component. Two
  started in a row run side by side.
* `session_result {"session": "...", "wait_secs": 20}` says how it stands (still running, waiting for a
  person, or its answer and what it cost) and waits up to `wait_secs`, at most 60. `session` may be left
  out when the agent has only one. `session_close` stops it.
* `budget_usd` is a ceiling the daemon enforces; `auto_approve` is which of the session's tools
  (`list_dir`, `read_file`, `write_file`, `run`) run without asking, none by default.
* A tool call the session is waiting on shows up as an ordinary approval (`session:write_file`,
  the agent that started it, `session <id>` in its chain), so the console, the chat bridge and
  `GET /v1/approvals` show it as they show any. `POST /v1/approvals/<id>` with
  `{"approve": true, "scope": "task"|"session"}` says how far a yes reaches. `GET /sessions` lists the sessions this runtime started.
* `--agentd-url`, `--agentd-token-file` and `--session-model` (or `HOLON_AGENTD_URL`,
  `HOLON_AGENTD_TOKEN_FILE`, `HOLON_SESSION_MODEL`) say where the daemon is and which model sessions use.
  The console starts a daemon itself. Without one the tools say sessions are not available.

## Tracing

Standards, end to end:

* **W3C Trace Context.** Every run, model call and tool call has a span id; a
  chain shares one 32-hex trace id. `traceparent` is accepted on
  `GET|POST /agents/<n>/run` and `POST /events/<topic>` (the run joins the
  caller's trace) and returned on the response. The lattice gateway forwards it
  both ways. What a run wakes is a child of the *tool call* that woke it.
* **OpenTelemetry.** Set `OTEL_EXPORTER_OTLP_ENDPOINT` (or `--otlp-endpoint`)
  and every finished run is exported to `<endpoint>/v1/traces` as OTLP/HTTP
  JSON: an `invoke_agent` span, with `chat` and `execute_tool` children, using
  the GenAI semantic conventions (`gen_ai.operation.name`, `gen_ai.agent.name`,
  `gen_ai.request.model`, `gen_ai.usage.*_tokens`, `gen_ai.tool.name`) plus
  `holon.*` attributes (trigger, hops, chain, run status).
* **No collector needed to look.** `GET /traces/<trace-id>` (admin) returns the
  same document, which any OTLP-aware viewer can open; `?format=runs` gives
  plain run records. The console's **trace** tab draws the tree.

## HTTP

The contract is [`api/holon/runtime/v1/runtime.proto`](../api/holon/runtime/v1/runtime.proto)
(ADR-0120), served as REST/JSON + SSE under `/v1`: ids in the path, the rest in a JSON body, query
strings only for `limit` and `after_seq`, errors as RFC 9457 problem details.

Open (loopback; these *are* the triggers):

* `POST /v1/agents/<n>/runs` `{"input", "why"?, "project"?, "attachments"?, "wait"?}`: with `wait`,
  the finished run; without, `202` at once and the run on the stream.
* `POST /v1/topics/<topic>/events` `{"payload"}`.
* The webhook shortcuts, for curl, cron and the gateway: `GET|POST /agents/<n>/run?q=…` (the body or
  `q` is the prompt, the answer is text), `POST /events/<topic>`, `GET /agents/<n>/ping`.

Admin (`Authorization: Bearer $(cat <state-dir>/admin-token)`):

* `GET /v1/agents`, `GET|PUT|DELETE /v1/agents/<n>` (`PUT` takes `{"spec"}`; a delete moves the agent
  to a trash for 30 days), `POST /v1/agents/<n>:pause|:resume`, `GET /v1/agents/<n>/runs?limit=`.
* `GET /v1/events?after_seq=` (SSE: `id` is the seq, `Last-Event-ID` resumes) and `GET /v1/state`
  (what is running and waiting now, and the seq it reflects).
* `GET /v1/approvals`, `POST /v1/approvals/<id>`.
* `GET /v1/projects`, `PUT /v1/projects/<n>`, `PUT|DELETE /v1/projects/<n>/agents/<agent>`.
* `GET /v1/keys`, `PUT|DELETE /v1/keys/<name>`, `PUT|DELETE /v1/keys/<key>/agents/<agent>`.
* Not in the contract yet: `GET /trash`, `POST /trash/<n>/restore`, `DELETE /trash/<n>`,
  `POST /projects/<n>/archive|unarchive`, `DELETE /projects/<n>`, `GET /agents/<n>/memory`,
  `POST /agents/<n>/inbox`, `GET /traces/<trace-id>[?format=runs]`, `GET|PUT /store/<ns>[/<key>]`,
  `GET /topics[/<topic>?after=N]`, `GET /sessions`, `/speech`, `/embed`.

## The gateway

`gateway/` is a wasm component that forwards `GET|POST /` to
`<runtime-url>/agents/<agent>/run`, with `runtime-url` and `agent` from the
deployment's `wasi:config`. The caller picks only the task text, never the
path, so one agent's hostname cannot reach another agent or the admin API. It
passes `traceparent` / `tracestate` through and returns the run's `traceparent`.
The runtime's address must be in the tenant's egress list, which the operator
grants through `platform-domain`'s `default-egress` config (ADR-0008 holds: a
tenant never authors its own egress).

## Food photos (Jev)

With `HOLON_JEV_KEY_FILE` naming a TypeSafe key, an agent can be granted `food_photo`, `food_pool` and
`food_correct`. Jev looks at the picture itself (the photo is shrunk to 800 px first: cost grows with
pixels) and answers four typed questions in one request: is there food, which dish from the user's
**pool** it is (each pool entry offered with its own reference photo), what kind of dish, and a kcal
band. A match is counted again; a miss is **added to the pool** with its kind, an estimated kcal and
the photo as a reference. Every photo is logged as a meal.

Per user, in the shared store under `food.<user>`: `pool/f<n>` and `meal/<unix time>`. Reference
photos are in the agent's workspace (`food-pool/`). Estimates are marked until `food_correct` gives
the real kcal or a better name. The photo is sent to TypeSafe; Jev's image support is a preview.

## Google Calendar

Tools an agent can be granted by name: `gcal_calendars`, `gcal_list`, `gcal_add`, `gcal_update`,
`gcal_delete`. They use the owner's own Google login, from files read at the moment they are needed
(never in a spec): `~/.comp-secrets/google/client.json` (the OAuth client Google gave) and
`token.json` (written by `agent-runtime google-auth`, mode 0600). `--google-dir` / `HOLON_GOOGLE_DIR`
changes the directory. The three that change anything wait for the owner's approval (a poll in Matrix).

* `gcal_list` takes `from`, `to` (default a week), `calendar` (a name, `all`, or nothing for the
  main one); local dates. One event per line with its id, so a later call can change it. An event's
  **description is never returned**: it is text anyone who invited you wrote.
* `gcal_add` takes `title`, `start` (`2026-10-15T06:30`, or a date alone for an all-day event), `end`
  (default an hour later), `calendar`, `notes`, `location`. No invitations are sent. A read-only
  calendar is refused before Google is asked.
* `gcal_update` changes only the fields given; moving an event keeps its length. `gcal_delete`
  reads the event back before deleting it.

**Setup, once (about five minutes in the Google Cloud Console):**

1. Create a project and enable the **Google Calendar API**.
2. OAuth consent screen: **External**, scope `https://www.googleapis.com/auth/calendar`. For a
   personal account set the publishing status to **In production** (an "unverified app" notice is
   fine for yourself): in *Testing*, Google expires the refresh token after 7 days.
3. Credentials, *Create credentials*, **OAuth client ID**, type **Desktop app**; download the JSON to
   `~/.comp-secrets/google/client.json` (`chmod 600`).
4. `agent-runtime google-auth`: a browser opens, you allow access, and the login is saved. It uses a
   loopback redirect with PKCE; nothing is sent anywhere but Google.

Unlike the macOS `holon-calendar` connector this needs no macOS permission and works wherever the
runtime does. It can also change and delete events, which that connector cannot.

## Not built yet

* Argument schemas derived from a capability's `wit` ref, and a Jev check that
  a declared capability plausibly serves the description (`capability-advisor`
  is the pattern). Today `wit` is stored and shown to the model, not enforced.
* A lattice-backed `Bus` and store (the `event-bus` / `knowledge-memory`
  components over NATS). `Bus` is a trait and `Kv` is one type behind one lock,
  so it is a local swap; today both are files, single process.
* Catch-up for cron ticks missed while the runtime was down (bus events are
  caught up; schedules are not).
* Streaming replies, and tool use through `llm:inference` (ADR-0102 explains
  why the model call is native until WASI 0.3 streams reach `comp-host`).
