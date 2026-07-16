# Helpdesk bench (round 12) — full app path on the native host, NATS vs memory KV

The first bench of a whole *app* request path (HELPDESK.md rung 1) rather than
per-capability calls: every request below goes browser → hyper → wasmtime →
`helpdesk_domain.composed.wasm` (helpdesk-domain + auth-guard + record-store +
fsm-workflow + id-generate + markdown) → `wasi:keyvalue` backend.

- Host: `just host-helpdesk` (vet-host binary, release), Apple M4 (10 cores), macOS
- Load: `oha -z 10s -c 20` (login/spa 5s), localhost
- Backends: NATS = JetStream KV in Docker on the same machine (durable,
  disk-persisted per write); memory = in-process HashMap
- Tokens: pre-minted session; login is the only unauthenticated write

| route | work per request | NATS rps | NATS p50/p99 ms | memory rps | mem p50/p99 ms |
|---|---|--:|--:|--:|--:|
| `GET /api/tickets` (agent) | introspect + list page | 282 | 71.5 / 107 | 2 065 | 9.8 / 14.5 |
| `GET /api/tickets/{id}` | introspect + record + messages find-by + md→html ×N | 161 | 125.9 / 185 | 2 135 | 9.6 / 14.4 |
| `POST /api/tickets` | introspect + create(indexed) + FSM instance + message create(indexed) | 55 | 368.2 / 568 | 2 196 | 9.2 / 14.4 |
| `POST /auth/login` | argon2 verify + session mint | 107 | 190.8 / 302 | 249 | 81.8 / 124.5 |
| `GET /` (static SPA) | host fs, no wasm | 98 355 | 0.19 / 0.57 | — | — |

## Takeaways

- **The component is not the cost.** With the memory backend every ticket
  route sits at ~2.1k rps / ~10 ms p50 at c=20 — list, detail, and the
  multi-write create are indistinguishable, so the wasm + composition +
  auth introspection overhead is flat and small.
- **On NATS, cost = number of KV round-trips.** Each `wasi:keyvalue` op is a
  synchronous JetStream round-trip (disk-persisted on write). Create does the
  most ops (ticket record + 2 index writes + FSM definition/instance +
  message + index) → 55 rps / 368 ms p50; list does the fewest → 282 rps.
  The gap to memory (~10–40×) is storage latency, not the app.
- **Login is argon2-bound either way** (~80 ms of hashing at this
  concurrency); NATS only adds the session write on top. Same shape as the
  auth-guard rounds (see HOST-PERF.md).
- **Static SPA from the host bypasses wasm entirely** — ~98k rps for
  `index.html`, so serving the frontend from `--static-dir` is free.
- **Path to faster NATS numbers** (not taken — rung 1): batch the per-request
  KV ops (record-store already uses `wasi:keyvalue/batch` for reads), cache
  session introspection host-side, or run the host next to NATS. The
  `wasi:keyvalue` contract has no CAS/append, so index maintenance is
  read-modify-write by design — same finding as round HOST-PERF.

## Repro

```bash
docker compose -f infra/compose.yaml up -d nats
just host-helpdesk            # NATS-backed on :3007 (+ SPA)
# memory variant: same binary with --kv memory
oha -z 10s -c 20 -H "authorization: Bearer $TOKEN" http://127.0.0.1:3007/api/tickets
```
