# ADR-0120 — The runtime speaks a contract too

*Element, the board and a webhook drive the same agents, and each of them learned the runtime's
API by reading its handler.*

**Status: accepted, and served** as REST/JSON + SSE under `/v1`. The contract is
[`api/holon/runtime/v1/runtime.proto`](../../api/holon/runtime/v1/runtime.proto), its REST twin
[`api/holon/runtime/v1/openapi.yaml`](../../api/holon/runtime/v1/openapi.yaml);
`agent-runtime/tests/contract.rs` compiles the first, `agent-runtime/src/v1.rs` serves both. gRPC is not
served yet: see the steps at the end.

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
- **REST/JSON + SSE, and gRPC from the same schema, on one port**, as `comp-agentd` serves them: ids in
  the path, the rest in a JSON body, query strings only for paging and cursors; enums lowercase, as
  `comp-agentd`'s REST says them. Versioned under `/v1`; errors are RFC 9457 problem details; money is
  integer micro-USD.
- **One event envelope**, `RuntimeEvent {seq, at_ms, kind}`. `seq` is per runtime, monotonic and gapless;
  `GET /v1/events?after_seq=` resumes (SSE `id` is the seq); `GET /v1/state` is the snapshot with the
  seq it reflects ([ADR-0107](0107-a-client-catches-up-from-a-snapshot-then-tails.md)'s pattern). Unknown
  kinds are ignored, so a new one is additive.
- **The webhook shortcuts stay**: `GET|POST /agents/<n>/run?q=`, `POST /events/<topic>` and
  `GET /agents/<n>/ping`, for curl, cron and the gateway component, documented as that and nothing more.
- **The agent spec is a `Struct` for now.** It is still moving; the fields every client reads (`name`,
  `description`, `paused`, the model's label, projects) are typed beside it.

**Not gRPC-only, and not gRPC first.** Every client today but one is Rust in this repository, and that
one, the gateway, can only speak `wasi:http`. gRPC is served from the same contract when a client outside
this repository wants it, at no extra design cost.

## What does not change

wRPC over NATS inside the lattice; `comp-agentd`'s contract; the platform's API, which can take the same
conventions later; and the providers outside (models, Concept2, Matrix), which speak what they speak.

## Steps

1. This ADR and the schema, compiled by a test. *Done.*
2. `/v1` served beside the old routes, on the server the runtime already had (`tiny_http`): REST/JSON
   and SSE need nothing else, and moving to `axum` + `tonic` is the step that brings gRPC, taken with the
   first client that wants it. *Done.*
3. The event log carries `seq` (`Sequenced` in `agent-wire`; the last 10,000 events in memory, seqs from
   the clock so a restart continues past every earlier one); `/v1/events` and `/v1/state` replace
   `/stream`. The board resumes from `after_seq`; the bridge is woken by the stream instead of polling
   runs and approvals every few seconds. *Done.*
4. The bridge and the board use `/v1`; the routes it replaces are gone. Still on their old routes, until
   they join the schema: the trash, archiving and deleting a project, memory, the inbox, traces, the
   store, topics, sessions, speech and embeddings. *Done for what the schema covers.*
