# Goal 13: Pure System 1 JEV Wire Codec in crates/agent-wire

**Traces to:** ADR-0124.

## What is wanted

1. **Move Codec into `crates/agent-wire`**:
   - Create `crates/agent-wire/src/jev.rs` housing `QuestionSpec`, `AnswerSpec`, `QuestionItem`, `evaluate_body`, and `parse_evaluate`.
   - Export `pub mod jev;` in `crates/agent-wire/src/lib.rs`.
2. **Clean Up `agent-matrix/src/jev.rs`**:
   - Remove `#[path = "../../components/typesafe-provider/src/codec.rs"]`.
   - Import types and functions from `agent_wire::jev`.
3. **Refactor `agent-runtime/src/jev.rs`**:
   - Replace manual `serde_json::Value` parsing in `noul()`, `choice()`, and `score()` with the typed parsing from `agent_wire::jev`.
4. **Update `components/typesafe-provider`**:
   - Re-export or consume `agent_wire::jev` to avoid codec drift.

## Acceptance Gate

- `cargo test -p agent-wire` verifies JEV evaluation request and response codecs.
- `cargo test -p agent-matrix -p agent-runtime` passes 100% with zero `#[path = ...]` compiler attributes in `agent-matrix/src/jev.rs`.
