# Agent memory recall benchmark (ADR-0104)

Question: where should per-scope vector recall for autonomous agents run, and how
fast can it be? Three candidates, all 256-dim unit vectors (synthetic: 10
Gaussian clusters + noise 0.8, so **latency numbers are solid, recall numbers are
pessimistic/optimistic in ways real embeddings will change**). Apple M2 Max, 12
cores, 2026-10-08. Single run each; p50 over 20–40 queries; no warm-up tuning.

| dir | what it measures |
|---|---|
| `localbench/` | the **real** `agent_runtime::store::Store::recall_semantic` (and lexical `recall`) over `memory/<a>.jsonl` + `<a>.vecs.jsonl` |
| `surreal_bench.py` | SurrealDB v3.1.3 (`docker run ... memory`, 4 CPU, 8 GiB) over HTTP `/sql`, keep-alive, bearer token. Raw rows: `surreal_results.jsonl` |
| `flat/` | an in-process exact scan in Rust (f32 and int8), 1 and 8 threads, plus the `instant-distance` HNSW crate |

Run: `cargo run --release` in `localbench/` and `flat/` (`RUSTFLAGS="-C target-cpu=native"`),
`python3 surreal_bench.py <port> memory base,scale,skew,nohnsw,conc,auth out.jsonl`.

## Results

### One agent's private memory, as built (`Store::recall_semantic`)

| entries | semantic p50 | lexical p50 | sidecar |
|---|---|---|---|
| 100 | 1.2 ms | 0.12 ms | 0.3 MB |
| 1 000 | 7.5 ms | 0.56 ms | 3.2 MB |
| 10 000 | 75 ms | 5.4 ms | 32 MB |
| 100 000 | 770 ms | 55 ms | 321 MB |

Linear, ~7.5 µs/entry. The cost is parsing the whole JSONL (256 floats per line)
on every call, not the cosine math.

### After: the in-memory index in `Store` (this PR), warm calls

`localbench/` again, same data, after `Store::recall_semantic` keeps vectors in memory
and re-reads only appended bytes. p95 is the one-time cold load of each agent per process.

| entries | before p50 | after p50 | speed-up | cold load (p95) |
|---|---|---|---|---|
| 1 000 | 7.5 ms | 0.07 ms | 107× | 13 ms |
| 10 000 | 75 ms | 0.28 ms | 270× | 81 ms |
| 100 000 | 770 ms | 2.9 ms | 265× | 813 ms |

### End to end with the real embedding model (`agent-runtime/tests/recall_e2e.rs`)

`google/embeddinggemma-2` via `embed/server.py` (on the Apple GPU, MPS), 8 facts +
1 000 distractors, paraphrased questions, release build:

| | before | after |
|---|---|---|
| top-1 correct | 8/8 | 8/8 |
| query embedding p50 | 40 ms | 40 ms |
| search p50 | 7.6 ms | 0.23 ms |
| save-then-recall (embeds 1 memory) | 46 ms | 41 ms |
| first recall, embeds 1 008 memories | 3.6 s | 3.6 s |

Embedding one query costs 37–40 ms (MPS; 62 ms on CPU) and a document ≈ 4 ms in a
batch of 32+. So **a recall tool call is now ~40 ms and the model is the floor**; the
index matters most once memories reach the thousands (at 10 000 the old path adds 75 ms,
the new one 0.3 ms). Further reduction would be a smaller/faster query model or a
cache of repeated query embeddings; neither is done.

### The same recall, exact, vectors held in memory, as a ceiling (`flat/`)

| entries | f32, 1 thread | f32, 8 threads | int8, 1 thread (recall@10) | RAM |
|---|---|---|---|---|
| 1 000 | 0.10 ms | 0.25 ms | 0.04 ms (0.94) | 1 MB |
| 10 000 | 0.31 ms | 0.17 ms | 0.09 ms (0.91) | 10 MB |
| 100 000 | 2.7 ms | 1.0 ms | 0.87 ms (0.85) | 102 MB |
| 200 000 | 5.4 ms | 1.9 ms | 1.7 ms (0.85) | 205 MB |
| 1 000 000 | 26 ms | 9.5 ms | 8.7 ms (0.80) | 1 GB |

Naive int8 loses recall on this data; not worth it. 128-dim halves the time.
`instant-distance` HNSW: build 1.4 s / 39 s / 99 s at 10k / 100k / 200k, query
0.1–0.7 ms, **recall@10 1.00 / 0.93 / 0.82**. Not worth it below ~1M vectors.

### SurrealDB v3.1.3

HTTP round trip for `RETURN 1`: **0.29 ms with a bearer token, 14.6 ms with HTTP Basic
(root:root)**. `components/knowledge-graph/src/lib.rs:100` sends Basic, so every query
the component issues carries that cost.

Dense query as written in `knowledge-memory` (`vec <|k,COSINE|> q AND scope='x'`) —
**this is SurrealDB's brute-force KNN; it does not use the HNSW index**:

| rows in table (10 equal scopes) | rows in the scope | filtered, p50 | exact `ORDER BY` p50 | unfiltered p50 |
|---|---|---|---|---|
| 1 000 | 100 | 2.3 ms | 4.4 ms | 18 ms |
| 10 000 | 1 000 | 17 ms | 31 ms | 110 ms |
| 50 000 | 5 000 | 74 ms | 119 ms | 507 ms |
| 100 000 | 10 000 | 126 ms | 183 ms | 1 037 ms |
| 200 000 | 20 000 | 237 ms | 319 ms | 2 122 ms |

Skewed (100 020 rows: one scope 90 000, then 5 000 / 500 / 20, plus 45 of 100):

| scope rows | filtered p50 | exact scan p50 |
|---|---|---|
| 90 000 | 1 027 ms | 1 424 ms |
| 5 000 | 75 ms | 124 ms |
| 500 | 10 ms | 16 ms |
| 20 | 0.9 ms | 1.6 ms |

≈ 11–20 µs per scanned row, i.e. 20–40× slower than the in-process scan. Forcing the
HNSW path (`<|10,ef|>`, ef 40–400) with the same scope filter **post-filters**: 500-row
scope recall@10 0.31 at ef=40 and 0.08 at ef≥100 (1.4–7 s), 20-row scope returned
5.8 of 10 rows. A scope filter cannot be combined with the HNSW index here.

Throughput, 100k rows (10 000 per scope), 4 CPUs: 1 thread 7 qps (p50 136 ms),
4/16/32 threads all ≈ 22 qps (p50 177 / 723 / 1 459 ms) — CPU-bound, no scaling.
Mixed 8 readers + 4 writers (10-row inserts, 100k table): read p50 375 ms, 175 rows/s
written. Bulk insert alone ≈ 5 000 rows/s with the HNSW index defined.

Container memory reached 7.7 GiB of 8 GiB after loading all datasets (in-memory
engine, HNSW indexes defined on each).

### Not measured

- SurrealDB with RocksDB/SurrealKV storage, and a persistent deployment.
- The wasm `knowledge-memory` component and its sparse + RRF + hydration steps
  (it issues several queries per recall, each paying the round trip).
- Real-embedding quality beyond the 8-question end-to-end check above (a labelled
  set would be needed for recall@k).
