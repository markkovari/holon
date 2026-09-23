# jco-ai

`ai:inference` is a set of **domain AI verbs** — `summarize`, `classify`, `extract`,
`generate`, `rewrite`, `embed` — expressed over the provider-agnostic
`llm:inference` boundary. The `ai-inference` component builds prompts and validates
results; it never talks to a specific model. Any component that exports
`llm:inference` can satisfy it.

This example runs the **MOCK** provider: a deterministic, offline
`llm-inference` component composed into `ai-inference` with
[`wac plug`](../../reconciler/src/plug.rs) via:

```sh
cargo xtask stage-examples   # ai-inference + llm-inference -> ai_inference.composed.wasm, copied here
```

The result, `ai_inference.composed.wasm`, exports **only**
`ai:inference/assistant@0.1.0` — the `llm:inference` import is satisfied internally.
Because the provider is the mock, every call is deterministic:

- `summarize` → `"Summary: " + first 80 chars`
- `classify` → the **first** label, confidence `1000`
- `extract` → one `mock-<field>` pair per requested field
- `generate` / `rewrite` → echoes the input (`"mock: ..."`)
- `embed` → a fixed 8-dim `f32` vector

Swap in a real provider component (e.g. one wrapping an HTTP LLM) and recompose:
nothing in `ai-inference` or the calling app changes.

## Run

```sh
(cd ../.. && cargo xtask stage-examples)   # build + stage the .wasm this transpiles
npm install
npm test          # transpiles ai_assist.composed.wasm -> gen/, runs test/ai.test.ts
```

`jco transpile` needs **no `--map`**: the only remaining imports are
`wasi:cli` / `wasi:io` / `wasi:filesystem` / `wasi:clocks`, all auto-shimmed by
`@bytecodealliance/preview2-shim`.
