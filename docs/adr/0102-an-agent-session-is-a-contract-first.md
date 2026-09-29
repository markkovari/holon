# ADR-0102 — an agent session is a contract first

*A Telegram bot, a Node backend and a Python script should all be able to
drive the same agent, and none of them should have to parse a CLI's output.*

**Status: proposed.** Names the wire contract before any of it is served, the
way ADR-0099 named holon-vcs's shape before `crates/holon-vcs` existed. The
contract is `api/holon/v1/agent.proto` (ConnectRPC/gRPC) and its REST twin
`api/openapi.yaml` (JSON + Server-Sent Events).

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

## What has to exist first

- `components/llm-inference/wit/inference.wit` has no streaming and no tool use; real
  `text_delta` and tool events need both.
- `reconciler/src/cost.rs` is still unimplemented; `cost_usd_micros` is its
  output.
- The server that serves this contract, and where it runs (native per
  ADR-0095, or a component behind the host).

Deliberately left out: cancelling one task without closing its session. Add it
when a caller needs to keep a session after stopping a task.
