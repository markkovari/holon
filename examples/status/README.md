# status — status page / uptime monitor (e2e)

The [docs/apps/STATUS.md](../../docs/apps/STATUS.md) showcase as one composed wasm HTTP component on
the native Rust host. The timer-driven axis: the workload originates from
`sched:timer`, not an inbound request — each monitor is a recurring check job,
and `POST /api/tick` is the explicit pump (wasip2 has no background tasks).

## Run it

```bash
just host-status      # from repo root; status page on http://127.0.0.1:3012
```

`POST /api/monitors {name, url, period}` (period ≥ 10s), then click **Run
checks** (or `POST /api/tick`): each monitor probes its target and the page
shows up / degraded / down. A dead target degrades on the first failure and goes
down on the second consecutive one; `/api/monitors/{id}/history` is the fsm
transition log.

## Test it

```bash
just e2e-status       # composes + builds host + runs tests/status.rs
```

Proves: a self-probe (targeting the page's own root) stays **up**; a dead-port
monitor goes **up → degraded** on one failure and **degraded → down** on a
second consecutive failure, with both hops in the transition log. Slow (~12s):
monitors have a 10s minimum period, so the test sleeps across a period to force
the second probe.

## What's composed

`status-page` (`status:app`) imports only contracts:

- `sched:timer` — the recurring check jobs (due/lease)
- `records:store` — monitor config + last-probe snapshot
- `fsm:workflow` — per-monitor up/degraded/down + transition history
- `event:bus` — state-change fan-out (consumer-group poll)
- `notify:dispatch` — alert webhook on transition

plus host WASI: `wasi:http/outgoing-handler` (the probes), `wasi:keyvalue`,
`wasi:clocks`. No auth.
