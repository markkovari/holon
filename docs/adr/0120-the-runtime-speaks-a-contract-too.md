# ADR-0120 — The runtime speaks a contract too

*Element, the board and a webhook drive the same agents, and each of them learned the runtime's
API by reading its handler.*

**Status: proposed.** The contract is [`api/holon/runtime/v1/runtime.proto`](../../api/holon/runtime/v1/runtime.proto);
`agent-runtime/tests/contract.rs` compiles it. Nothing serves it yet: the steps are at the end.

## Where the wires are

| Between | Wire | Contract |
|---|---|---|
| components, across nodes | wRPC over NATS ([ADR-0028](0028-cross-node-calls-are-wrpc.md)) | the WIT |
| the reconciler and the hosts | NATS subjects `comp.v1.<lattice>.cmd.*` | the `lattice` traits |
| a caller and `comp-agentd` | REST + SSE and gRPC on one port ([ADR-0102](0102-an-agent-session-is-a-contract-first.md)) | `agent.proto`, `openapi.yaml` |
| a caller and the platform | HTTP/JSON | partly (a wasm component: HTTP is what it can serve) |
| **a caller and `agent-runtime`** | **HTTP/1.1, hand-rolled** | **none** |

The last row is the one every face of the agents uses: the Matrix bridge, the board, the console and the
gateway component in a tenant's sandbox. It grew one route at a time, and it shows:

- **No schema.** 62 handlers answer plain text, 24 JSON; an error is free text and a status code. The
  bridge and the board each parse what they read, and a changed shape fails at the first call, not at
  build time.
- **The input in two places.** A run's prompt is the body, or `?q=`; what the run is about rides in
  `?why=`, `?project=`, `?attach=`. A prompt in a URL is in every access log and proxy on the way.
- **A stream that drops.** `GET /stream` is a line of JSON per event with no sequence: a board that
  reconnects misses what happened meanwhile, and the bridge polls `/agents/*/runs` every few seconds
  because there was nothing to resume from.
- **Two styles for one kind of thing.** A Telegram bot in Python can drive a coding session properly
  (ADR-0102) and cannot drive an agent.

The query strings were not careless: the open tier (`/agents/<n>/run`, `/events/<topic>`) is meant for
anything that can poke an agent, curl and a cron job included, and the raw prompt as the body needs no
escaping. That reason still holds for one shortcut, not for the API.

## The decision

**The runtime gets a contract like `comp-agentd`'s, in the same tree, under the same conventions.**

- **One schema**, `holon.runtime.v1.AgentRuntimeService`: agents, runs, one event stream and a state
  snapshot, approvals, projects and API keys. Its messages are `crates/agent-wire`'s types field for
  field, so what the runtime emits today maps without loss; it imports `holon.v1.ApprovalScope` rather
  than a copy, because a yes reaches as far in either.
- **REST/JSON + SSE and gRPC, on one port**, as `comp-agentd` serves them: ids in the path, the rest in a
  JSON body, query strings only for paging and cursors. Versioned under `/v1`; errors are RFC 9457
  problem details; money is integer micro-USD.
- **One event envelope**, `RuntimeEvent {seq, at_ms, kind}`. `seq` is per runtime, monotonic and gapless;
  `GET /v1/events?after_seq=` resumes (SSE `id` is the seq); `GET /v1/state` is the snapshot with the
  seq it reflects ([ADR-0107](0107-a-client-catches-up-from-a-snapshot-then-tails.md)'s pattern). Unknown
  kinds are ignored, so a new one is additive.
- **One shortcut stays**: `GET /agents/<n>/run?q=` for a webhook, documented as that and nothing more.
- **The agent spec is a `Struct` for now.** It is still moving; the fields every client reads (`name`,
  `description`, `paused`, the model's label, projects) are typed beside it.

**Not gRPC-only, and not gRPC first.** Every client today but one is Rust in this repository, and that
one, the gateway, can only speak `wasi:http`. gRPC is served from the same contract when a client outside
this repository wants it, at no extra design cost.

## What does not change

wRPC over NATS inside the lattice; `comp-agentd`'s contract; the platform's API, which can take the same
conventions later; and the providers outside (models, Concept2, Matrix), which speak what they speak.

## Steps

1. This ADR and the schema, compiled by a test. *(here)*
2. The runtime's server moves from `tiny_http` to `axum` + `tonic`, the stack `comp-agentd` already uses in
   this workspace, and serves `/v1` beside the old routes.
3. The event log carries `seq`; `/v1/events` and `/v1/state` replace `/stream`. The board resumes from
   `after_seq`; the bridge follows the stream instead of polling runs.
4. The bridge, the board, the console and the gateway move to `/v1` through one client; the old routes
   go, but the webhook shortcut. The remaining handlers (store, memory, trash, traces, speech, inbox,
   sessions) join the schema as they move.
