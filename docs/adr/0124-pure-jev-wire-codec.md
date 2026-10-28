# ADR-0124 — Pure System 1 JEV Wire Codec in crates/agent-wire

*JEV System 1 evaluation is duplicated between a compiler `#[path = ...]` hack in `agent-matrix` and manual string JSON parsing in `agent-runtime`; moving the pure codec into `crates/agent-wire` unifies evaluation logic without compiler hacks.*

**Status: accepted.**

## The problem

JEV (TypeSafe System 1) evaluates fast, typed questions (gate probabilities, choices, scores) to classify incoming messages and photos without running heavyweight generative LLMs:
- `agent-matrix/src/jev.rs` borrows codec types by compiling across workspace boundaries with `#[path = "../../components/typesafe-provider/src/codec.rs"]`. This breaks independent crate compilation, makes IDE analysis fragile, and violates Rust modularity guidelines.
- `agent-runtime/src/jev.rs` duplicates JEV HTTP client logic and parses responses through untyped `serde_json::Value` traversals (`answers.get(id)?.get("choice")?...`).
- `components/typesafe-provider` maintains the original codec.

## The decision

1. **Host JEV wire types and serializers in `crates/agent-wire::jev`.**
   - Move `QuestionSpec`, `AnswerSpec`, `QuestionItem`, `evaluate_body`, and `parse_evaluate` into `crates/agent-wire/src/jev.rs`.
   - Maintain pure, no-I/O data transformations in the shared wire crate.
2. **Remove the `#[path = ...]` compiler hack from `agent-matrix/src/jev.rs`.**
   - `agent-matrix` imports `agent_wire::jev::{...}` directly.
3. **Harmonize `agent-runtime/src/jev.rs`.**
   - The runtime's Jev client delegates response parsing to `agent_wire::jev`, eliminating manual field extraction and ensuring identical confidence thresholding across both Matrix and HTTP triggers.

## What it does not do

- It does not introduce network dependencies to `crates/agent-wire`: HTTP transports remain in the caller (`agent-matrix`, `agent-runtime`, or `typesafe-provider`).
- It does not modify JEV's external REST API contract.
