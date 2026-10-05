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
  UTC, `@hourly`, `@every 30s`), `event` (`emit_event` from another agent, or
  `POST /events/<topic>`; the payload is the task).
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

Open (loopback; these *are* the triggers): `GET|POST /agents/<n>/run?q=…`,
`GET /agents/<n>/ping` (no model call), `POST /events/<topic>`.

Admin (`Authorization: Bearer $(cat <state-dir>/admin-token)`): `GET /agents`,
`PUT|GET|DELETE /agents/<n>`, `POST /agents/<n>/pause|resume`,
`GET /agents/<n>/runs|memory`, `GET /traces/<trace-id>[?format=runs]`,
`GET|PUT /store/<ns>[/<key>]`, `GET /topics[/<topic>?after=N]`,
`GET /approvals`, `POST /approvals/<id>/approve|deny`.

## The gateway

`gateway/` is a wasm component that forwards `GET|POST /` to
`<runtime-url>/agents/<agent>/run`, with `runtime-url` and `agent` from the
deployment's `wasi:config`. The caller picks only the task text, never the
path, so one agent's hostname cannot reach another agent or the admin API. It
passes `traceparent` / `tracestate` through and returns the run's `traceparent`.
The runtime's address must be in the tenant's egress list, which the operator
grants through `platform-domain`'s `default-egress` config (ADR-0008 holds: a
tenant never authors its own egress).

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
