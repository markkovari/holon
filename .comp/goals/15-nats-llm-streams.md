# Goal 15: NATS JetStream Backed Multi-Tier LLM Queuing and Coalescing

**Traces to:** ADR-0126.

## What is wanted

1. **NATS JetStream Ingestion Adapter**:
   - Enable `llm-batcher` (or an edge daemon) to ingest prompts from NATS subject `holon.llm.batch.>`.
   - Support publishing tasks with metadata: `{id, task_type, prompt, system_prompt, max_tokens, reply_subject}`.
2. **Coalesced Multi-Task Batch Execution**:
   - Consume messages from the `LLM_BATCH` WorkQueue stream.
   - Format multiple tasks into one structured multi-turn context (e.g. up to 100 tasks or 500k tokens per call).
   - Dispatch to `llm-router` targeting the Gemini Flash free pool (1 RPM cost).
   - Split and deliver individual task responses back to their respective reply subjects.
3. **JetStream Status & Dead-Letter Handling**:
   - Unparseable or failing tasks are moved to `holon.llm.batch.dlq` after 3 retries.
   - Realtime tasks on `holon.llm.realtime.>` bypass the batch queue and resolve immediately.

## Acceptance Gate

- Verification on `malna` showing messages published to `holon.llm.batch.test` being drained, coalesced, processed by Gemini Flash, and results published to response subjects.
- Confirmed RPM savings: 50 batched tasks processed in a single API call consuming 1 RPM.
