# pipeline — reliable event delivery, live

A live board for the **at-least-once event pipeline** (see [`PIPELINE.md`](../../PIPELINE.md)):
enqueue an event, watch it march **Pending → In-flight → Done**; take the
downstream sink **down** and watch events retry, back off, and drop to the
**dead-letter tray**; click **Replay** and watch a dead event re-enter the
pipeline — all over a held-open **SSE** stream, no refresh.

One composed wasm HTTP component (`pipeline-domain` + `outbox` + `event-bus` +
`id-generate`) on the native Rust host. The reliability core is entirely the
`outbox:dispatch` contract; the domain just pumps it and streams each
transition.

## Run it

```bash
just host-pipeline          # compose + serve on http://127.0.0.1:3016
```

Open the page:

1. **Enqueue event** (or **Burst ×10**) — cards appear in *Pending*, flip
   through *In-flight*, land in *Done* as the relay dispatches them.
2. Click **Sink up** to take it **DOWN** — now enqueue: events retry (the try
   counter climbs), then fall into the **dead-letter tray**.
3. Bring the sink back **up**, hit **Replay** on a dead card — it re-enters the
   pipeline and delivers.

## Test it

```bash
just e2e-pipeline           # enqueue→ack, live SSE, then down→retry→dead→replay
```

The e2e sets `CFG_MAX_ATTEMPTS=1` + `CFG_BASE_BACKOFF=1` so the dead-letter path
is reachable in a couple of seconds (host defaults are 5 attempts / 5s backoff).
