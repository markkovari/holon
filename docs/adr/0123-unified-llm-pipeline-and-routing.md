# ADR-0123 — Unified LLM Pipeline and Direct Free-Tier Routing

*Model inference is fragmented across four separate implementations; unifying them into `crates/holon-llm` and `components/llm-router` eliminates redundant fallback loops, retires loose Node.js shims, and adds native Gemini support.*

**Status: accepted.**

## The problem

Model calling in Holon evolved piecemeal across different components and layers:
1. `crates/holon-llm` (ADR-0116) formats messages for Anthropic and OpenAI-compatible endpoints as pure data without I/O.
2. `agent-runtime/src/model.rs` contains its own synchronous `ureq` HTTP transport and a bespoke `ModelSpec::Fallback` retry loop.
3. `reconciler/daemons/agentd/src/model.rs` contains another streaming SSE turn loop with separate error handling.
4. `tools/*-shim.mjs` (`gemini-shim.mjs`, `claude-shim.mjs`, `openai-shim.mjs`) spawn external Node.js child processes wrapping local CLIs to translate protocols, violating the rule against loose non-Rust dev tooling.
5. `components/llm-router` (PR #477) was introduced to handle intra-tier free-tier bubbling (Gemini Flash, Groq, Cerebras, Ollama) and proactive token-bucket rate limiting.

This fragmentation means four different places parse errors, retry on 429 rate limits, and construct wire formats.

## The decision

1. **Standardize all model codecs in `crates/holon-llm`.**
   - Add native Gemini API support (`/v1beta/models/...:generateContent` with system instructions, safety settings, and multimodal parts) alongside Anthropic and OpenAI formats.
   - Support Gemini's OpenAI-compatible endpoint (`/v1beta/openai/`) natively with zero wrapper shims.
2. **Centralize routing and bubbling in `components/llm-router`.**
   - `agent-runtime` and `comp-agentd` route completions through `llm-router` via standard WIT (`holon:llm/complete`) or HTTP proxy rather than managing separate provider fallback chains.
   - Intra-tier bubbling (Gemini Flash -> Groq -> Cerebras -> local Ollama) is resolved in one place.
3. **Retire loose Node.js shims in `tools/`.**
   - Delete `gemini-shim.mjs`, `claude-shim.mjs`, `openai-shim.mjs`, and `gemini-cli.mjs`. All model interactions are pure Rust within `crates/holon-llm` or WASM guests.

## What it does not do

- It does not force all inference to be cloud-only: local models (Ollama, Apple `fm`, `mlx_lm`) remain first-class members of Tier 1.
- Streaming SSE remains the transport contract for interactive sessions (`comp-agentd`), while unary completions remain the contract for background batch tasks.
