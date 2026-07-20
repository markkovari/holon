# Pulse bench (round 15) — sustained connections, a new dimension

Every other bench in this repo measures requests/sec. This one measures the
thing that makes `pulse` a different *class*: **concurrent held-open SSE
connections**, and whether one posted message fans out to all of them. Every
request goes browser → hyper → wasmtime → `pulse_domain.composed.wasm`
(pulse-domain + record-store + event-bus + id-generate) → `wasi:keyvalue`.

- Host: `just host-pulse` (vet-host, release), Apple M4 (10 cores), macOS
- Point ops: `oha -c 20` (8s). Fan-out: N `curl -sN` SSE readers held open, one
  broadcast posted, count how many received it. `bench/pulse-bench.sh [memory|nats]`
- Backends: NATS = JetStream KV in Docker; memory = in-process HashMap

| path | NATS | memory |
|---|--:|--:|
| `POST /messages` | 105 rps · 194 ms p50 | 4 103 rps · 4.9 ms p50 |
| `GET /messages` (20-msg history) | 67 rps · 303 ms p50 | 4 011 rps · 4.9 ms p50 |
| **fan-out: concurrent SSE connections that got one broadcast** | — | **150 / 150** |

## Takeaways

- **The new dimension is connections, not rps.** One `POST /messages` fanned out
  to **150 simultaneously held-open SSE streams**, every one delivering the
  message — on a host that instantiates the component *per request*. Each live
  stream is its own long-running wasm instance polling the log and writing
  frames; 150 of them coexist. That's the "sustained connection" class none of
  the other showcases touch, and it works on plain wasip2 (no WebSocket, no
  wasip3 async) because the host streams the body while the guest keeps running.
- **Posting is fast and flat** (4.1k rps memory / 105 NATS) — the same
  append-a-record cost as every other write, and the same ~40× memory:NATS gap
  (one is a HashMap insert, the other a JetStream round-trip).
- **Fan-out latency = the poll cadence.** A connected client sees a new message
  within its `POLL_MS` (700 ms) tick — the deliberate sleep that keeps each
  stream from busy-spinning. Lower it for snappier delivery, raise it for less
  KV load per connection; it's the one knob.
- **Cost per connection scales with poll cadence × backend.** Each of the 150
  streams polls the log every 700 ms; on memory that's free, on NATS it's a
  round-trip per stream per tick — so sustained-connection *count* on NATS is
  bounded by (connections × 1/POLL_MS) KV ops/s, not by rps. The multi-host
  upgrade (`event-bus` + `event-pusher` push instead of per-stream poll) is the
  way past that — noted, not built.

## Repro

```bash
docker compose -f infra/compose.yaml up -d nats     # for the NATS column
just compose-pulse && (cd host && cargo build --release --bin vet-host)
bench/pulse-bench.sh memory
bench/pulse-bench.sh nats
```
