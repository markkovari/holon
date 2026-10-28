# llm-router — `holon:llm-router@0.1.0`

A **smart cascading LLM inference router** that implements the vendor-agnostic [`llm:inference`](../llm-inference) export by bubbling requests across free-tier provider pools before gracefully falling back to sub-cent or frontier models.

## Why this exists

Single-provider architectures either exhaust free rate limits immediately (hitting HTTP 429) or burn money needlessly on paid APIs for routine tasks. `llm-router` solves both:

1. **Intra-Tier Pool Bubbling**: Aggregates multiple free-tier providers (Google Gemini Flash, Groq, Cerebras, and Local Ollama) into a single logical pool with ~75+ RPM combined capacity at $0.00.
2. **Proactive Token Bucketing**: Tracks per-provider RPM in monotonic memory. If a provider's quota is exhausted, the router skips it immediately without incurring the network delay of an HTTP 429 rejection.
3. **Circuit Breaking**: Uses an integrated circuit breaker (`failure_threshold: 2`, `cooldown: 60s`). If a provider reports 429 or 5xx, it trips to `OPEN` and routes traffic to healthy peers.
4. **Cascading Tiers**:
   - **Tier 1 (Free Pool)**: Gemini 2.5 Flash (15 RPM), Groq Llama 3.3 (30 RPM), Cerebras (30 RPM), Ollama (unmetered).
   - **Tier 2 (Sub-Cent Paid)**: DeepSeek-V3 / R1 (`$0.14/1M`).
   - **Tier 3 (Frontier Fallback)**: OpenAI GPT-4o / Anthropic Claude.

## The swap point

`llm-router` exports `llm:inference/inference@0.1.0`. Any component in Holon that imports inference can compose directly with `llm-router` via `comp-plug` or `wac plug`:

```bash
# Compose any domain component with llm-router
comp-plug ai-inference --plug components/target/wasm32-wasip2/release/llm_router.wasm \
  -o components/target/ai_inference.routed.composed.wasm
```

## Runtime Configuration (`wasi:config/store`)

| Key | Default | Description |
|---|---|---|
| `router:gemini:base` | `https://generativelanguage.googleapis.com/v1beta/openai` | Gemini API base |
| `router:gemini:model` | `gemini-2.5-flash` | Default Gemini model |
| `router:gemini:rpm` | `15` | Gemini RPM rate limit |
| `router:groq:base` | `https://api.groq.com/openai/v1` | Groq API base |
| `router:groq:model` | `llama-3.3-70b-versatile` | Default Groq model |
| `router:groq:rpm` | `30` | Groq RPM rate limit |
| `router:cerebras:base` | `https://api.cerebras.ai/v1` | Cerebras API base |
| `router:cerebras:model` | `llama3.1-8b` | Default Cerebras model |
| `router:cerebras:rpm` | `30` | Cerebras RPM rate limit |
| `router:deepseek:base` | `https://api.deepseek.com/v1` | DeepSeek API base |
| `router:deepseek:model` | `deepseek-chat` | Default DeepSeek model |

## Secrets (`comp:secrets/reader`)

Keys are read through the platform secret boundary (`comp:secrets/reader`) or fallback to environment:
- `gemini-api-key`
- `groq-api-key`
- `cerebras-api-key`
- `deepseek-api-key`
- `openai-api-key`
