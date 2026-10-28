# Goal 14: Decouple Domain Capabilities from Agent Runtime Kernel

**Traces to:** ADR-0125.

## What is wanted

1. **Pluggable Tool Handler Abstraction in `agent-runtime`**:
   - Introduce a dynamic `ToolHandler` registration trait in `agent-runtime/src/agent.rs` so tools are not hardcoded inside a monolithic `match name` block.
2. **Componentize Food Recognition**:
   - Extract `agent-runtime/src/food.rs` into `components/food-domain`.
   - Implement tool handler forwarding to the component.
3. **Componentize Google Calendar**:
   - Move `agent-runtime/src/google.rs` into `components/gcal-provider` (or an external tool plugin).
   - Keep OAuth2 flow in a separate helper binary/plugin instead of `agent-runtime google-auth`.
4. **Prune `agent-runtime` Binary**:
   - Remove `food.rs` and `google.rs` from `agent-runtime/src/`.
   - Remove hardcoded fields from `Runtime` / `Host`.

## Acceptance Gate

- `agent-runtime/src/` contains zero domain-specific references (`food`, `gcal`).
- `cargo test -p agent-runtime` passes with dynamic tool handlers.
- Total binary size and compilation time of `agent-runtime` decreases.
