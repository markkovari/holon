# llm-batcher — `holon:llm-batcher@0.1.0`

A **multi-task batch coalescer and queue drainer** for LLMs.

## Why this exists

Free-tier LLM providers (such as Google Gemini Flash) offer high token ceilings (1,000,000 TPM) but strict request limits (15 RPM). Processing background tasks (data extraction, summarization, triage) as individual calls burns through rate limits within seconds.

`llm-batcher` solves this by:
1. **Queuing Deferred & Batch Work**: Accepts tasks asynchronously over WIT (`batch-queue/enqueue`) or HTTP (`POST /api/batch`).
2. **High-Context Coalescing**: Gathers up to $N$ pending tasks into a single multi-task prompt:
   ```json
   [
     {"id": "task-1", "task": "Translate ..."},
     {"id": "task-2", "task": "Summarize ..."}
   ]
   ```
3. **Valley-Filling Drain**: Submits 1 single LLM inference call to `llm:inference` (composed with `llm-router`), consuming just 1 RPM to process dozens of tasks simultaneously.
4. **Result Unpacking**: Maps each response entry back to its task ID and caches the answer for retrieval via `poll` or `GET /api/batch/{id}`.

## HTTP Endpoints

| Method | Endpoint | Description |
|---|---|---|
| `POST` | `/api/batch` | Enqueue a task: `{"id": "optional", "prompt": "...", "system": "..."}` |
| `GET` | `/api/batch/{id}` | Poll task status: returns `{"status":"pending"}` or `{"status":"completed","text":"..."}` |
| `POST` | `/api/batch/drain` | Drain and coalesce up to 20 pending items into 1 LLM call |

## WIT Composition

```wit
world llm-batcher {
    export batch-queue;
    export wasi:http/incoming-handler@0.2.0;

    import llm:inference/inference@0.1.0;
    import wasi:keyvalue/store@0.2.0-draft;
    import wasi:clocks/monotonic-clock@0.2.0;
}
```
Compose with `llm-router` to ensure the coalesced calls flow through the free provider pool.
