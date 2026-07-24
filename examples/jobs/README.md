# jobs — durable background-job queue (JOBS.md)

A durable job queue with scheduling, retry/backoff, dead-lettering, and replay —
and a **swappable execution backend** (`durable:workflow`: in-process by default,
Golem-provider for crash-resumable runs). See [JOBS.md](../../JOBS.md) for the
full write-up.

Like the other composed HTTP showcases, jobs runs on the native Rust host, so
this directory holds the board SPA + a Rust e2e (not a jco harness).

```
public/index.html        # the live board (Queued / Running / Done / Dead-letter)
tests/jobs.rs            # e2e: done / retry-then-succeed / DLQ / replay / exactly-once
```

## Run

```bash
# from the repo root:
just host-jobs           # composes jobs-domain (+ outbox + inproc-workflow + cron
                         # + idempotency + records); board on http://127.0.0.1:3038
```

Enqueue from the toolbar: **Email** (succeeds), **Flaky** (fails then succeeds —
watch the attempt count climb), **Boom** (dead-letters), **Delayed 3s**, or
**Burst ×5**. The board self-ticks over SSE, so jobs advance on their own; dead
jobs get a **Replay** button.

```bash
just e2e-jobs            # the lifecycle e2e (spawns the host)
```
