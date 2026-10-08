# ADR-0104 — agents share what they know, per project

*Autonomous agents each remember alone. A project's agents (and the people in its
Matrix Space) should be able to find what any member learned, without a new
permission model and without one agent being able to plant beliefs in the others.*

**Status: proposed.** Nothing here is built. Written first because the scope
boundary, the write rule and the embedding contract are all expensive to reverse
once agents depend on them.

## What exists, read from the code

| piece | where | what it is | limit |
|---|---|---|---|
| private recall | `agent-runtime/src/store.rs:248-316` | `remember`/`recall` per agent. Lexical overlap, plus brute-force cosine over `memory/<agent>/vecs.jsonl`, vectors keyed by text | linear scan; no model version; per agent; single-process files behind one lock |
| project blackboard | `agent-runtime/src/projects.rs:36`, `agent.rs:674` (`resolve_ns`), `agent.rs:803-840` | `store_get/put/list` on `project.<name>`, granted by membership (`effective()`), versioned, `by` recorded | key-value only: you must already know the key. No search |
| knowledge pool | `components/knowledge-memory` (ADR-0081, 0084) | SurrealDB HNSW + TF-IDF, RRF fusion, outcome weighting, dedup key, `observe`/`promote` split at the linker | built for the coding loop (`goal`, `env`, `attempt`, `score`, interface `tags`); client is `reconciler/src/memory.rs`; the loop is currently *paused* (README), and retrieval into prompts is unwired (slices 2–3) |
| embedding | `agent-runtime/src/embed.rs`, `embed/server.py` | local sentence-transformers service, optional, lazily started; callers fall back to lexical | one model, not versioned in the record |
| egress | `agent.rs:705` `http_get` | allow-list per agent | this is what keeps an agent from calling a memory service it was not granted |

The access model the feature needs (membership is the grant) is already built.
The retrieval layer is not, and the one that exists is shaped for another job.

## The decision

### 1. Three scopes, and only these

| scope | readable by | writable by | in a prompt |
|---|---|---|---|
| `agent:<name>` | that agent | that agent | as today |
| `project:<name>` | members of the project | members, via `observe` only | on request, labelled untrusted |
| `curated:<name>` | members of the project | **a person, or an explicit promotion step** | labelled, ranked first |

There is no global scope. A pool across unrelated projects returns plausible
neighbours from the wrong context, and the one thing membership-as-grant buys is
that a personal agent's calendar facts cannot appear in a work project. If a fact
should span projects, it is copied into each by a person.

`curated` is the analogue of ADR-0084's `patterns`: what the project *believes*.
The coding loop promotes on a passing gate. Autonomous agents have no gate, so
promotion is **never an agent verb**: it is a Matrix command from the project's
owner (`!keep <id>`), or the HTTP admin API. An agent can propose (`observe`
with `proposed: true`), which only puts the entry in a review list.

### 2. Scope is enforced in agent-runtime, not in the component

`knowledge-memory` cannot know which agent is calling; it trusts its caller.
`agent-runtime` can: it already derives grants from the registry per run. So:

- Scope strings are **never taken from tool arguments**. The tool surface is
  `recall(query, scope?)` and `observe(text, scope?)`, where `scope` is
  `private` (default) or a project *name* the agent is a member of. The runtime
  resolves it through `spec.projects`/`spec.store` exactly as `resolve_ns` does
  today, and rejects anything else with the same "you may X: ..." error.
- Recall with no scope reads `agent:<me>` plus every project the agent belongs
  to, merged by rank. This is the default because a member that has to *ask*
  for shared knowledge will not.
- The memory service is **not reachable by the agent**: it is not on any
  agent's `http_get` allow-list, and the runtime holds the only client. The
  runtime is the trusted party; the component stays policy-free about identity.

### 3. Reuse `knowledge-memory`, don't write a second index

Run the existing component and call it from `agent-runtime` over HTTP, the way
`reconciler/src/memory.rs` already does. ADR-0095 reserves "native" for what a
wasm guest cannot do (cron, open approvals); storage and retrieval policy is not
one of those. This needs a WIT bump to `knowledge:memory@0.3.0`, additive:

- `entry` and `recall-opts` gain `scope: string` (empty = today's behaviour).
  Rows are filtered on it in the SurrealQL `WHERE`, **before** the HNSW
  `<|k,COSINE|>` result is used, so a scope can never be widened by the index.
  (To confirm in a spike: that filtered KNN returns k hits and not k-then-filtered.)
- `entry` gains `author: string`, `source: string` (Matrix event id or run id),
  `model: string` (embedding model id) and `proposed: bool`.
- The coding-loop fields (`goal`, `env`, `attempt`, `score`, `tags`) stay and
  are left empty by agents. `namespace` keeps `patterns | solutions | errors`;
  agents write `solutions`/`errors`-shaped *observations*, and `curated`
  scope entries are stored as `patterns` with the project scope. `observe`
  keeps refusing `patterns`; `promotion` stays linked only into the review path.

Not changed: the dedup key (re-learning reinforces one row), `attribute`
(outcomes are the only thing that moves standing), `RRF_K`, the 900-character
cap, and the "error means do the work" rule (an unreachable pool must never
fail a run; recall falls back to today's private lexical recall).

### 4. What counts as an outcome for an agent

Coding runs have a gate. Agent runs mostly do not, so `attribute` needs a
signal that is not the agent's own opinion (ADR-0081: no self-reported
confidence). Candidates, in order of trust:

1. an owner reaction on the Matrix message that cited the entry (👍/👎),
2. a run that read the entry finished without `must_call` failures or an
   approval rejection,
3. nothing — the entry decays by age.

Start with 1 and 3. Do not start with 2: a run that "succeeded" while quoting a
wrong fact would reinforce it.

### 5. Retrieved text is data

Entries from `project` scope are written by other agents, some of which read
the web (`http_get`) or Matrix messages from outside. Retrieved text goes into
the prompt in a fenced block that says it is recalled notes, not instructions,
with author and age. `curated` is visually separate and listed first. This is
mitigation, not a guarantee, which is why writes are `observe` only and
promotion needs a person.

### 6. Provenance and drift

Every row carries `author`, `source`, `at` and the embedding `model`. A
canary is not built (ADR-0084's ponytail): instead, a recall that sees rows
whose `model` differs from the current one **reports it** and the runtime
re-embeds those rows lazily (it has the text). HNSW `DIMENSION` is fixed
per index, so the embedding model for shared memory is chosen once; the
default is the 256-dim model `agent-runtime/embed` already serves.

### 7. Migration of private memory

Private `memory/<agent>/` keeps working unchanged and remains the fallback when
the pool is unconfigured, so `--surreal-url` stays optional exactly as it is for
the coding loop. A one-shot importer copies existing `memory/*/vecs.jsonl` into
`agent:<name>` scope (vectors reused when the model matches). No shared entries
are created by migration.

## Spike result (2026-10-08, SurrealDB v3.1.3, in-memory, Docker on a laptop)

Question: does `WHERE vec <|k,COSINE|> q AND scope = 'x'` filter *before* the
cut, or take the global top-k and then drop rows (which would return too few
rows and make a scope a recall hole)?

**Correctness: filtered.** 200 rows in scope B all closer to the query than 5
rows in scope A; a KNN with `scope='A'` returned all 5 A rows and no B rows, for
k=5 and k=10 (k=10 returned 5, the scope's size). Then 256-dim vectors, 10
scopes, 1k / 10k / 50k rows, a plain index on `scope` plus the HNSW index:

| rows | filtered KNN p50 | unfiltered KNN p50 | exact scan of the scope p50 | scope leaks | recall@10 vs exact |
|---|---|---|---|---|---|
| 1 000 | 18 ms | 32 ms | 20 ms | 0 | 10/10 |
| 10 000 | 35 ms | 125 ms | 47 ms | 0 | 10/10 |
| 50 000 | 92 ms | 521 ms | 138 ms | 0 | 10/10 |

Reading it:

- The scope filter is safe to rely on: no leaks, no recall loss, on this data.
  The "scope enforced in SQL" design in §3 stands. It still gets a scenario test
  in `scenarios.rs`, and the filter stays in the query, never in post-processing.
- **The win is the scope index, not HNSW.** Per-scope pools are small, and an
  exact scan of one scope is within 1.5× of the filtered KNN. Latencies include
  the HTTP round trip and a laptop container, so treat them as an order of
  magnitude, not a benchmark.
- **Per-turn recall for a *private* pool should not go through this path.** A
  1k-row private memory costs ~18 ms here; the in-process JSONL scan is
  microseconds to a few ms at that size (not measured here). So: `agent:<name>`
  recall stays local and unchanged; the service is for `project:` and
  `curated:` scopes, and recall merges both. This amends §2: unscoped recall
  reads the local private store *and* the pool, in parallel.
- Not tested: concurrent writers, a different filter selectivity (one scope
  holding most rows), and a real wasm component in front. Repeat the
  `knowledge-memory` scenario suite with a `scope` column before WIT 0.3.0.

## Phasing

1. **Spike — done** (above). Remaining: measure the JSONL scan for comparison,
   and test skewed scopes and concurrent writers.
2. **WIT 0.3.0 + component.** Fields above, scenario tests for scope isolation
   (an `agent:a` row is invisible to a recall for `agent:b`, to an unscoped
   recall, and to a project it was not written in).
3. **agent-runtime client.** `src/pool.rs` modeled on `reconciler/src/memory.rs`;
   `recall`/`observe` gain the optional `scope`; `resolve_scope` mirrors
   `resolve_ns`; fallback to current behaviour when absent.
4. **Matrix.** `!keep`, `!forget`, a review list for proposals, reaction →
   `attribute`.
5. **Shared with the coding loop.** Wire ADR-0084 slices 2–3 against the same
   pool so a `curated` project fact can also reach a goal run's prompt.

## Consequences

- One retrieval implementation serves the coding loop and the agents, and the
  unfinished ADR-0084 work is no longer orphaned.
- agent-runtime gains an optional dependency on SurrealDB (through the
  component). Deployments that do not run it behave as today.
- The enforcement point is the runtime. A bug in `resolve_scope` leaks across
  projects, so it gets the same test shape as `resolve_ns` and `effective()`.
- Shared memory is long-lived and uncurated by default; the cost of `observe`
  being cheap is a review queue someone has to read.

## Alternatives

- **One global shared pool.** Rejected: cross-project leakage, and relevance
  falls as unrelated corpora mix. Membership already gives the boundary.
- **A second, native vector store in agent-runtime** (usearch/hnsw_rs, or
  ruvector). Rejected for now: duplicates `knowledge-memory`'s hybrid
  retrieval, dedup and attribution, and leaves the coding loop with a different
  system. Revisit only if the spike shows the HTTP + SurrealDB path is too slow
  for per-turn recall; the likely candidate then is a maintained HNSW crate
  behind the same `Pool` trait, not an external agent-memory product.
- **Extend `project.<name>` blackboard with embeddings.** Rejected: it is a
  versioned KV with compare-and-set semantics (`if_version`); mixing ANN search
  into it changes what it promises. Agents still use it for coordination.
- **Index Matrix room history automatically.** Rejected as the default: noisy,
  and it turns every message, including from outside the project, into
  retrievable text. Possible later as an opt-in `source` with its own scope.
- **Let agents promote after N positive outcomes.** Rejected: outcomes for
  agents are weak signals, and promotion is the poisoning boundary
  (ADR-0084). A person promotes.

## Open questions

- Does the memory component run per host or one shared instance for several
  runtimes? Single-process JSONL files stop being safe the moment two runtimes
  write, which is the main reason this should not stay on files.
- Per-project retention: TTL by default, or only by explicit `!forget`?
- Should `curated` be exportable to a repo file (reviewable in git), so the
  project's beliefs are diffable? Attractive; not needed for v1.
