# ADR-0125 — Decoupled Domain Capabilities from Agent Runtime Kernel

*Domain-specific implementations (Food recognition and Google Calendar OAuth2) are compiled directly into `agent-runtime`, bloating the kernel and violating Holon's capability isolation model; extracting them into pluggable WASM components restores runtime purity.*

**Status: accepted.**

## The problem

Holon's core premise is that capabilities belong in isolated WASM components (ADR-0005, ADR-0023, ADR-0095), composed dynamically based on what an application imports.

However, during early prototyping:
1. `agent-runtime/src/food.rs` (616 lines) hardcoded food photo classification, dish pools, and category taxonomies (`salad`, `pizza`, `soup or stew`) into the runtime core.
2. `agent-runtime/src/google.rs` (1,028 lines) hardcoded Google Calendar OAuth2 PKCE token flows, credential storage, and calendar CRUD.
3. `agent-runtime/src/agent.rs` hardcoded dispatch arms (`food_photo`, `food_pool`, `food_correct`, `gcal_calendars`, `gcal_list`, `gcal_add`, `gcal_update`, `gcal_delete`) directly inside the agent execution loop.

This couples the agent runtime daemon to specific external web APIs and specific meal tracking semantics, bloating binary size and violating capability partitioning.

## The decision

1. **Extract Food domain to a WASM component (`components/food-domain`).**
   - Define WIT interface `holon:food/pool` and `holon:food/recognize`.
   - The agent runtime dispatches tool calls via standard component links or external tool descriptors rather than compiled-in functions.
2. **Extract Google Calendar to a capability provider (`components/gcal-provider` or external tool plugin).**
   - Move OAuth2 consent and token refresh out of `agent-runtime` into a dedicated provider daemon or component.
   - The runtime accesses calendar tools through standard tool registration rather than hardcoded `host.google()`.
3. **Slim `agent-runtime` to core mechanics.**
   - The runtime kernel contains only: session state, memory retrieval, trigger scheduling (cron/webhooks/Matrix), and pluggable tool execution.

## What it does not do

- It does not break existing agents: existing tool names (`gcal_list`, `food_photo`) continue to be honored through the tool registry.
- It does not require changes to agents' prompt specs.
