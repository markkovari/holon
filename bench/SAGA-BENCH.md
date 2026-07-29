# Saga bench (round 14) — a durable workflow path, memory vs NATS KV

The first bench of a **stateful workflow** path rather than stateless CRUD. Every
request goes browser → hyper → wasmtime → `saga_domain.composed.wasm`
(saga-domain + fsm-workflow + record-store + idempotency-guard + event-bus +
id-generate + scheduler-timer) → `wasi:keyvalue`.

- Host: `just host-saga` (comp-host, release), Apple M4 (10 cores), macOS
- Load: `oha -c 20` (8s) for the point reads/writes; a sequential loop for the
  full-saga latency (each iteration `POST /trips` then `POST /trips/{id}/run`)
- Backends: NATS = JetStream KV in Docker on the same box; memory = in-process HashMap
- `bench/saga-bench.sh [memory|nats]`

| path | work | NATS | memory |
|---|---|--:|--:|
| `POST /trips` (start) | create saga record + fsm instance | 99 rps · 208 ms p50 | 2 492 rps · 8.1 ms p50 |
| `GET /trips/{id}` | record read + fsm history | 490 rps · 41 ms p50 | 2 364 rps · 8.5 ms p50 |
| **full saga → committed** | start + 3 legs (idempotency + booking + saga update + event each) + fsm commit | **976 ms/saga (1/s)** | **38.5 ms/saga (26/s)** |
| **full saga → compensated** | as above, then roll back 2 booked legs | 1 077 ms/saga | 38.4 ms/saga |

## Takeaways

- **A full saga is ~30 KV round-trips, and that's the whole story.** Committing a
  trip does: create the saga record, `fsm:create-instance`, then per leg an
  `idempotency:begin` + booking `create` + saga `update` + `event:publish`, then
  the `fsm:commit`. On memory that's 38 ms (26 sagas/s); on NATS each hop is a
  synchronous JetStream round-trip → 976 ms (1 saga/s). The ~25× gap is storage
  latency × op-count, exactly the shape every other round shows — the workflow
  logic itself is free (the memory column proves it).
- **Compensation is nearly the same cost as commit** (1 077 vs 976 ms on NATS):
  rolling back two booked legs is two more idempotency-fenced record deletes +
  events. Rollback isn't a special expensive path — it's the same primitives run
  backwards.
- **Point ops are flat and cheap** (`start`/`get` ~2.4k rps on memory), same band
  as conduit/helpdesk — the composition + wasm overhead is not the bottleneck.
- **`pump` is O(running sagas), not a fixed-cost op**, so it isn't in the table:
  it scans every `running`/`compensating` saga and advances each one step. That's
  fine at demo scale but scales with the live-saga backlog.
  - *Upgrade path (not taken — rung 3):* pump only sagas whose `sched:timer` is
    **due** (the timer index already exists), or shard the running set — turning
    an O(backlog) sweep into O(due). Same finding as the other rounds: the
    `wasi:keyvalue` contract has no server-side query, so "find work to do" is a
    scan unless an index/timer narrows it.
- **Durability is the point, and it's not in these numbers.** The reason a saga
  costs 30 round-trips instead of living in memory is that every step is
  persisted — which is exactly what lets it survive a host kill and resume (see
  `just durable-saga`). The latency *is* the durability.

## Repro

```bash
docker compose -f infra/compose.yaml up -d nats     # for the NATS column
just compose-saga && (cd host && cargo build --release --bin comp-host)
bench/saga-bench.sh memory
bench/saga-bench.sh nats
just durable-saga                                    # the restart-resume proof
```
