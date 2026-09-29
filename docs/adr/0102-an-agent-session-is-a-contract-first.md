# ADR-0102 — an agent session is a contract first

*A Telegram bot, a Node backend and a Python script should all be able to
drive the same agent, and none of them should have to parse a CLI's output.*

**Status: accepted, and the REST half is served.** The contract is
`api/holon/v1/agent.proto` (ConnectRPC/gRPC) and its REST twin
`api/openapi.yaml` (JSON + Server-Sent Events). `comp-agentd`
(`reconciler/src/bin/agentd.rs`) serves the REST twin; nothing serves the
proto yet.

## The problem

Today an agent run is a single shot: `holon goal run` or `goald` drives the
generation loop, and what a caller sees is exit codes (`goalexit.rs`), a trace
in SurrealDB (ADR-0092), and goal transitions on `/api/projects/{p}/events`.
That is enough for a queue drained by a daemon. It is not enough for a caller
that holds a conversation: it cannot watch text arrive, see which tool is
about to run, approve it, or know what the session has spent so far.

## The decision

A **session** fixes model, workspace and budget for its lifetime. A **task** is
one prompt sent to it; one task runs at a time (ADR-0072). Everything the agent
does is an **event** on one ordered stream per session:

- `task_started`, `text_delta`, `tool_call_started`, `tool_approval_required`,
  `tool_approval_resolved`, `tool_call_finished`, `usage_updated`,
  `task_completed`, `task_failed`.
- Each carries a per-session `seq`, monotonic with no gaps. It is the SSE `id`
  and the resume cursor: a caller that drops reconnects with `after_seq` (or
  `Last-Event-ID`) and loses nothing. Sending a task and reading the stream are
  separate calls, so a bot can fire a task and subscribe from anywhere.
- Unknown event kinds are ignored by clients, so new kinds are additive.

**Money is integer micro-USD.** `money.rs` counts cents, which is right for a
wallet and wrong for a single model call that costs a fraction of one. Token
counts split input, output, cache-read and cache-write, because they are
priced differently.

**Approval is per tool call.** A tool not in the session's
`auto_approve_tools` pauses the task on `tool_approval_required` until someone
answers; a denial's reason goes back to the model as the tool's error. This is
finer than the goal-level `awaiting-human` state in `platform-domain`, which
stays as it is.

## What this is not

[Myrmic](https://github.com/peeriot/myrmic) was read as a possible fabric for
this, and in place of NATS. It is a Wasm actor runtime for edge devices over
Zenoh — closer to `host/` than to `lattice/` — and it does not yet have what
`lattice` uses NATS for: durable streams, replicated KV with TTL, an object
store, request/reply, or authentication between nodes. It is also not
embeddable (the runtime ships as a daemon, GPL-2.0) and does not run on
macOS. The fabric already sits behind `Inventory`/`CommandBus`/`Artifacts`
with an in-memory second implementation, so a Zenoh backend stays a later,
local change if edge devices ever join the lattice.

## How it is served, and the exception it takes

`comp-agentd` is a native daemon, and it answers ADR-0095's third question
with a deliberate NO. The agent loop and the model call live inside it, next
to the held connections that are its reason to be native. It talks to
Anthropic's `/v1/messages` or any OpenAI-compatible `/chat/completions`
directly, streaming, with tools, instead of going through a provider
component. The reason is that the component path cannot do this yet:

- `llm:inference` (`components/llm-inference/wit/inference.wit`) has no tool
  use and no streaming.
- A WASI 0.2 guest cannot stream a reply across a component boundary, so
  `text_delta` from a provider component would be one delta per turn.

This is an exception with an exit. When WASI 0.3 streams are available to
`comp-host`, the model call moves behind a provider component that exports a
streaming, tool-using interface, and `comp-agentd` keeps only what must be
native: sessions, the SSE streams and the waits for approval.

What it does:
- Tools are `list_dir`, `read_file`, `write_file` and `run`
  (`agentd/tools.rs`). The file tools are confined to the session's
  workspace, with symlinks resolved. `run` is a shell and cannot be confined;
  approval is its boundary. By default every tool asks.
- Sessions must live under a `--workspace-root`.
- Cost comes from `cost.rs`'s price table, via the new `cost_usd_micros`.
  An unknown model is charged at the dearest tier, as `cost_cents` has always
  done. That includes a self-hosted one such as csatapaci's Qwen, whose budget
  therefore burns as if it were opus.
- `--provider mock` is scripted and free. It emits every event kind, for tests
  and for anyone building a client.

What it does not do yet:
- Persist anything. Sessions and their event logs are in memory and gone on
  restart. Moving the log to JetStream, the way `comp-park` holds its
  records, is the next step if it serves more than one user.
- Serve the proto. ConnectRPC/gRPC needs a codegen step this tree does not
  have.
- Cancel one task without closing its session. Add it when a caller needs
  to keep a session after stopping a task.
