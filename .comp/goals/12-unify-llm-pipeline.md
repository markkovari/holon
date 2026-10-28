# Goal 12: Unify LLM Pipeline and Direct Free-Tier Routing

**Traces to:** ADR-0116, ADR-0123.

## What is wanted

Eliminate redundant LLM calling logic across the platform by consolidating all wire encodings into `crates/holon-llm` and routing runtime requests through `components/llm-router`:

1. **Native Gemini Codec in `crates/holon-llm`**:
   - Implement `gemini.rs` in `crates/holon-llm` to serialize `Request` and deserialize `Reply` for Google's Generative AI REST API (`/v1beta/models/{model}:generateContent`).
   - Add unit tests verifying prompt serialization, system instruction mapping, multimodal inline image/audio part generation, and token usage parsing.
2. **Decommission Node.js Shims**:
   - Remove `tools/gemini-shim.mjs`, `tools/claude-shim.mjs`, `tools/openai-shim.mjs`, and `tools/gemini-cli.mjs`.
   - Update `tools/openai-shim.mjs` callers to target `llm-router` or direct provider endpoints.
3. **Consolidate `agent-runtime/src/model.rs`**:
   - Refactor `complete()` to route requests directly through `llm-router`'s bubbling mechanism, removing duplicate provider fallback loops in `agent-runtime`.

## Acceptance Gate

- `cargo test -p holon-llm` verifies Gemini, OpenAI, and Anthropic codecs without network access.
- `cargo test -p llm-router` verifies intra-tier bubbling across Gemini, Groq, Cerebras, and Ollama.
- Zero loose `.mjs` model shims remain in `tools/`.
