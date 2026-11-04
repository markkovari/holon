# ADR-0129 — Agent Handoff and Context Routing Protocol

*When agents communicate via unstructured raw string messages (`{"message": "..."}`), prompt token budgets are wasted inlining large artifacts and task intent is lost; request/response message envelopes with pass-by-reference context pointers eliminate context bloat and standardize inter-agent delegation.*

**Status: accepted.**

## The problem

1. **Context Bloat & Token Inefficiency**: When Agent A invokes Agent B (via `agent:<name>` or `spawn_task`), Agent A previously had only a single string argument: `"message": "..."`. If the task involved reviewing a 50KB code diff, analyzing a benchmark log, or inspecting shared state, Agent A had to either:
   - Inline thousands of tokens of raw text directly into `message`, quickly consuming model context windows and incurring heavy token costs on every turn.
   - Omit the context, forcing Agent B to re-query or guess where the data is stored.
2. **Missing Intent Contracts**: Plain strings lack structured intent metadata. A caller could not cleanly declare whether an invocation was a complete handoff (`delegate`), a read-only question (`query`), an evaluation (`review`), or a privileged tool execution (`execute`).
3. **No Structured Feedback or Return Context**: When Agent B completed its work, Agent A only received Agent B's raw model text reply. If Agent B created workspace files, modified blackboard KV store keys, or produced content-addressed artifacts, Agent A had no structured handle to those outputs without parsing free-form text.

## The decision

1. **Structured Envelopes in `crates/agent-wire`**:
   - Define `HandoffRequest` and `HandoffResponse` as core wire types in `crates/agent-wire/src/handoff.rs`, shared between native runtime and browser/wasm targets.
   - `HandoffIntent`: `delegate`, `query`, `review`, `execute`, `handoff`.
   - `HandoffStatus`: `success`, `declined`, `partial`, `error`.
2. **Pass-by-Reference Context Pointers (`ContextRefs`)**:
   - Callers point to existing knowledge and stored state rather than inlining raw payloads:
     - `store_refs`: Blackboard KV keys (e.g. `["project.alpha/analysis", "shared/schema"]`).
     - `workspace_refs`: Workspace file paths (e.g. `["reports/q3.md", "src/parser.rs"]`).
     - `memory_refs`: Agent memory keys, topics, or semantic queries.
     - `run_refs`: Prior correlated run IDs or session records.
     - `artifacts`: Content-addressed digests (OCI digests, git blob hashes).
3. **Ambient Prompt Resolution**:
   - When an agent is invoked via a handoff envelope, `agent-runtime` automatically formats an ambient context header in the target agent's initial prompt turn.
   - The target agent receives concise reference handles and fetches details on-demand using its standard capabilities (`store_get`, `read_file`, `recall`), keeping turn context small and fast.
4. **Produced Context Tracking (`ContextProduced`)**:
   - `agent-runtime` tracks artifacts and state produced during the delegated run:
     - Blackboard keys written (`store_put`).
     - Workspace files written or edited (`write_file`, `edit_file`).
     - Produced artifact IDs.
   - Returns these in `HandoffResponse.context_produced` alongside a concise natural language `summary`, tokens consumed, and optional structured JSON `output`.
5. **Contract First (Protobuf & OpenAPI Mirroring)**:
   - Mirror `HandoffRequest` and `HandoffResponse` in `api/holon/runtime/v1/runtime.proto` and `openapi.yaml`.
   - Expose `POST /v1/agents/{name}/handoff` on `agent-runtime`'s HTTP API.
6. **Backward Compatibility**:
   - Legacy invocations `agent:<target> {"message": "..."}` continue to work unmodified.
   - When callers provide `task`, `intent`, or `context`, `agent:<target>` automatically packs and executes the request as an enveloped handoff.
   - A dedicated `agent_handoff` tool is also available when granted.

## What it does not do

- It does not replace the durable event bus (`emit_event`, JetStream) for asynchronous broadcast pub/sub. Handoffs model direct synchronous or task-spawned delegation between cooperating agents.
- It does not bypass capability security boundaries: an agent can only delegate to agents for which it holds `agent:<target>` capability grants.
