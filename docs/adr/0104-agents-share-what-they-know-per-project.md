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

### 3. Retrieval runs in-process, per scope; SurrealDB is the shared store

*Revised after the benchmark (`bench/agent-memory/`); the first draft reused
`knowledge-memory` as the retrieval engine and was wrong about its cost.*

What the numbers say (256-dim, M2 Max, details in the bench README):

| per recall | 1 000 | 10 000 | 100 000 |
|---|---|---|---|
| today's private recall (`Store::recall_semantic`) | 7.5 ms | 75 ms | 770 ms |
| SurrealDB `<|k,COSINE|>` + scope filter (bearer auth) | 2–18 ms | 17–75 ms | 126 ms–1 s |
| in-process exact f32 scan, vectors in RAM | 0.10 ms | 0.31 ms | 2.7 ms |

- The dense query in `knowledge-memory` is a brute-force scan limited by a scope
  index. It never touches the HNSW index it defines, and the HNSW path cannot
  be combined with a scope filter (post-filters: recall 0.08–0.31, short rows).
  It is correct, and 20–40× slower than a scan done in-process; one 90k-row scope
  costs ~1 s and the server saturates near 22 qps on 4 CPUs.
- The current private recall is slow because it re-parses JSON per call, not
  because the math is expensive.

So the decision is:

- **Each scope is its own in-memory flat index in agent-runtime** (a contiguous
  `f32` matrix + row metadata), exact, single-threaded below ~50k rows,
  threaded above. A scope is a separate matrix, so isolation is structural:
  there is no filter to get wrong and nothing to post-filter. 100k entries is
  100 MB and 2.7 ms; 1M is 1 GB and 26 ms. No ANN index is needed at this scale.
- **Durability and sharing live in SurrealDB** (via `knowledge-memory`'s
  `observe` / `attribute` / `promote` policy, unchanged), for `project:` and
  `curated:` scopes. The runtime loads a scope's rows on first use and
  refreshes by version; private scopes keep the local files but move to a binary
  vector file so a load is one `read`, not a JSON parse.
- **`knowledge-memory`'s dense query stays for the coding loop**, where the
  pool is small and the wasm component is the thing being exercised. Two cheap
  fixes are due regardless: send a bearer token instead of HTTP Basic (saves
  ~14 ms per query), and stop defining an HNSW index that no query uses.
- Hybrid ranking (lexical + dense RRF, outcome weighting) is re-implemented
  over the in-memory scope, ranked the same way; the scenario suite from
  ADR-0084 is the oracle.

WIT 0.3.0 still gains `scope`, `author`, `source`, `model` and `proposed`
(for the stored rows and the coding loop's use of them). The `scope` column is
a `WHERE` on a plain index, never on a vector index.

Not changed: the dedup key (re-learning reinforces one row), `attribute`
(outcomes are the only thing that moves standing), `RRF_K`, the 900-character
cap, and the "error means do the work" rule (an unreachable pool must never
fail a run; recall falls back to the local private store).

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

## Benchmark result (2026-10-08–09)

Full tables and the scripts are in `bench/agent-memory/`. What changed in this
ADR because of it:

1. **The first spike conclusion was wrong.** It saw correct scope filtering and
   concluded filtered HNSW works. The query used, `<|k,COSINE|>`, is SurrealDB's
   brute-force KNN, so the HNSW index was never exercised; the real HNSW form
   (`<|k,ef|>`) post-filters and loses recall. What held up: a scan limited by a
   scope index never leaks across scopes.
2. **My estimate of the current local scan was wrong too.** I wrote "microseconds
   to a few ms"; it is 7.5 ms at 1 000 memories and 75 ms at 10 000.
3. **~14 ms of every SurrealDB number in the first spike was HTTP Basic auth**
   (password hashed per request); a bearer token costs 0.29 ms.
4. **In-process exact search is 20–250× faster than either**, and exact, so §3
   is rewritten around it.

Still unmeasured: query-embedding latency (likely the floor of a recall call),
persistent SurrealDB engines, the wasm component end to end, and recall quality
on real embeddings.

## Phasing

1. **Benchmark — done** (above). Remaining: embedding latency, persistent
   engine, real embeddings.
2. **In-memory scope index in agent-runtime** (`src/index.rs`): flat matrix,
   exact top-k, binary vector file, isolation tests (a scope's rows are
   invisible to every other scope's recall). This alone fixes private recall.
3. **Shared scopes.** `src/pool.rs` modeled on `reconciler/src/memory.rs`
   (bearer auth), WIT 0.3.0 fields, `resolve_scope` mirroring `resolve_ns`,
   load/refresh of a scope from SurrealDB; fallback to local when absent.
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

- Two runtimes holding the same project scope in RAM need a refresh signal
  (version poll, or a Surreal live query). Does the memory component run per host
  or one shared instance for several runtimes? Single-process JSONL files stop being safe the moment two runtimes
  write, which is the main reason this should not stay on files.
- Per-project retention: TTL by default, or only by explicit `!forget`?
- Should `curated` be exportable to a repo file (reviewable in git), so the
  project's beliefs are diffable? Attractive; not needed for v1.
