# ratelimit — a live throttle wall

A wall for the two limiter capabilities (see [`docs/apps/RATELIMIT.md`](../../docs/apps/RATELIMIT.md)):
hammer an endpoint and watch the **attempt bar** climb to its ceiling, the key
**LOCK** with a countdown, and the **quota gauge** drain — then recover when the
window elapses. Every verdict streams live over **SSE**.

One composed wasm HTTP component (`throttle-domain` + `ratelimit-guard` +
`quota-meter` + `event-bus` + `id-generate`) on the native Rust host. The
counters live entirely in the two limiter contracts; the domain is a thin
decision gate that streams each verdict.

## Run it

```bash
just host-ratelimit        # compose + serve on http://127.0.0.1:3020
```

Open the page (max 10 attempts / 15s window, quota 20 / 30s):

1. **Hit once** / **Burst ×10** / **Hold to hammer** — the attempt bar fills.
2. At the ceiling the badge flips to **LOCKED** with a live retry countdown;
   hits return 429 and show `locked` in the verdict stream.
3. The quota gauge drains independently on its own period.
4. Wait out the window (or hit **Reset**) — the wall re-opens.

## Test it

```bash
just e2e-ratelimit         # ceiling → 429, quota decrements, lockout + recovery, live SSE
```

The e2e (`CFG_MAX_ATTEMPTS=6`, `CFG_LOCKOUT_WINDOW=3`) proves N allowed then a
429 at the ceiling, a quota `remaining` that decrements, that enough failures
lock the key (observed via `state`) and it recovers after the window, and that a
verdict reaches a separate held-open SSE connection.
