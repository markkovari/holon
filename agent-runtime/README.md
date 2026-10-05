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

## HTTP

Open (loopback; these *are* the triggers): `GET|POST /agents/<n>/run?q=…`,
`GET /agents/<n>/ping` (no model call), `POST /events/<topic>`.

Admin (`Authorization: Bearer $(cat <state-dir>/admin-token)`): `GET /agents`,
`PUT|GET|DELETE /agents/<n>`, `POST /agents/<n>/pause|resume`,
`GET /agents/<n>/runs|memory`, `GET /approvals`,
`POST /approvals/<id>/approve|deny`.

## The gateway

`gateway/` is a wasm component that forwards `GET|POST /` to
`<runtime-url>/agents/<agent>/run`, with `runtime-url` and `agent` from the
deployment's `wasi:config`. The caller picks only the task text, never the
path, so one agent's hostname cannot reach another agent or the admin API. The
runtime's address must be in the tenant's egress list, which the operator
grants through `platform-domain`'s `default-egress` config (ADR-0008 holds: a
tenant never authors its own egress).

## Not built yet

* Argument schemas derived from a capability's `wit` ref, and a Jev check that
  a declared capability plausibly serves the description (`capability-advisor`
  is the pattern). Today `wit` is stored and shown to the model, not enforced.
* Catch-up for schedule ticks missed while the runtime was down.
* Streaming replies, and tool use through `llm:inference` (ADR-0102 explains
  why the model call is native until WASI 0.3 streams reach `comp-host`).
