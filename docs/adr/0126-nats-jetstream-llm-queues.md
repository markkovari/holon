# ADR-0126 — NATS JetStream Backed Multi-Tier LLM Queuing and Coalescing

*Connecting `llm-batcher` and agent task queues directly to NATS JetStream streams (`LLM_REALTIME` and `LLM_BATCH`) ensures durable distributed queuing across edge nodes, eliminates in-memory state loss, and preserves free-tier RPM limits via 1M-token coalescing.*

**Status: accepted.**

## The problem

1. **State volatility in guest memory**: `components/llm-batcher` was implemented with in-memory task queues. If a host node restarts, queued tasks waiting for coalescing are lost.
2. **Multi-node coordination**: Tasks originate from diverse nodes across the lattice (e.g. Raspberry Pi Zero 2 W edge sensors, Mac workstations, and Raspberry Pi 5 daemons). An in-memory queue on one node cannot batch tasks submitted from another node.
3. **Rate limit preservation**: Gemini's free tier allows 15 RPM and 1M tokens/minute. Non-realtime background workloads (daily digests, git commit summaries, photo triage, batch embeddings) consume full RPM if dispatched immediately, starving interactive realtime agents.

## The decision

1. **Leverage the dedicated NATS JetStream streams deployed on `malna`**:
   - `LLM_REALTIME`: In-memory backed, subject `holon.llm.realtime.>`, 60s TTL, limits retention. Handled via immediate request-reply bubbling by `llm-router`.
   - `LLM_BATCH`: NVMe file-backed WorkQueue, subject `holon.llm.batch.>`, 7-day retention. Holds non-realtime prompts across all nodes.
2. **NATS JetStream consumer in `llm-batcher` / daemon**:
   - `llm-batcher` pulls pending tasks from `holon.llm.batch.>` when batch triggers fire (either task count >= threshold or age >= window).
   - Tasks are concatenated into a single structured multi-prompt taking advantage of Gemini's 1M-token context window.
   - Only 1 RPM is consumed from the free pool to execute dozens or hundreds of tasks.
3. **Decoupled completion delivery**:
   - Completed task results are published back to unique return subjects (`holon.llm.reply.<task_id>`) or stored in JetStream key-value buckets, allowing callers to disconnect and retrieve results later.

## What it does not do

- Realtime interactive chats (Matrix bridges, GPUI console) do not go through the batch stream; they publish directly to `LLM_REALTIME` for immediate bubbling.
