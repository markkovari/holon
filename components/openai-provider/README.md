# openai-provider — `openai:provider@0.1.0`

A **concrete LLM provider** implementing the vendor-agnostic
[`llm:inference`](../llm-inference) boundary by talking to an
**OpenAI-compatible HTTP API**. This is the production counterpart to the
deterministic mock provider: same `llm:inference/inference` export, real
`/v1/chat/completions` + `/v1/embeddings` calls.

Works against anything that speaks the OpenAI JSON contract — OpenAI, Azure
OpenAI, Together, Groq, vLLM, or a local Ollama / llama.cpp `--api` server.

## The swap

`ai:inference` (the domain verbs — summarize/classify/extract/…) imports
`llm:inference/inference` and never names a vendor. Which provider answers is a
**composition choice**:

```bash
# offline / tests — the deterministic mock provider:
just compose-ai          # -> ai_inference.composed.wasm   (+ llm-inference mock)

# production — the real OpenAI-compatible client, SAME domain layer:
just compose-ai-openai   # -> ai_inference.openai.composed.wasm (+ openai-provider)
```

No `ai:inference` or app code changes between the two — only the `wac plug`
target. That is the whole point of the `llm:inference` boundary.

## Config (`wasi:config/runtime`)

| key | default | meaning |
|-----|---------|---------|
| `openai:base-url` | `https://api.openai.com/v1` | API base (point at Azure/Ollama/vLLM here) |
| `openai:api-key` | — | bearer token, sent as `Authorization: Bearer …` |
| `openai:model` | `gpt-4o-mini` | default chat model (overridable per call via `options.model`) |
| `openai:embed-model` | `text-embedding-3-small` | default embedding model |

`temperature` is milli-units on the wire (`700` → `0.7`); `0`-valued options are
omitted so the provider's own defaults apply.

## What is tested where

The codec — request shaping (`chat_body` / `embed_body`) and response parsing
(`parse_completion` / `parse_embedding`) — lives in `src/codec.rs` as **pure,
WASI-free functions** with host unit tests (9 tests: valid OpenAI request shape,
optional omission, JSON escaping, usage/finish-reason parsing, empty-content →
`no-content`, bad JSON → `bad-response`, embedding extraction). Run them in a
host crate that includes `codec.rs` (the component itself is `wasm32-wasip1`-only
and can't `cargo test` directly because of the wasm bindings).

The **live HTTP path is NOT driven in-process under jco**: jco backs outbound
HTTP with async `fetch`, but the provider blocks on a WASI pollable right after
`handle`, which deadlocks Node's single-threaded loop (the same constraint
documented in `examples/jco-notify`). A further blocker is wasi:io version skew
in the composed module (`0.2.0` + `0.2.3`), which a single shim can't span. The
end-to-end call is therefore validated under **wasmCloud** with a real
`wasi:http` httpclient provider + a `wasi:config` block — the codec tests cover
the request/response correctness that the in-process path would otherwise check.

## World

```wit
world openai-provider {
    export llm:inference/inference@0.1.0;
    import wasi:http/outgoing-handler@0.2.0;
    import wasi:config/runtime@0.2.0-draft;
}
```
