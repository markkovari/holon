# ADR-0132 — Park is a component

*`comp-park` and `comp-resume` held JetStream because a component could not; everything they
decided was already `holon-park`'s engine, which builds for wasm. The engine moves into
`park-store`, on the app's own keyvalue, and both daemons go.*

**Status: accepted.** Revises ADR-0100's and ADR-0106's service shape; the engine, the wire and the
recovery policy are unchanged.

## The problem

ADR-0100 made `comp-park` native for "a held NATS JetStream KV connection and a publish to
`PARK_WAKE`", and ADR-0106 made `comp-resume` native for "a held JetStream consumer and a timer".
Each was one adapter around an engine that is pure. Neither reason survives what the host now gives a
component:

- **Compare-and-set** is `comp:store/cas` (ADR-0065), on every keyvalue backend: JetStream's own
  revision on NATS, one transaction on SQLite.
- **The timer** is the host's pump (ADR-0130).
- **`PARK_WAKE`** was only ever a nudge: "the record is the truth, the stream is a nudge", and the
  recovery pass already re-announces whatever a lost nudge missed.

## The decision

- **`park-store` runs the engine in-process**, over a `ParkStore` on `comp:store/cas` with
  `comp-park`'s key layout. Every lot function is `holon_park::serve::dispatch`, the code
  `comp-park` served, so `park-gateway`'s `/v1/<function>` wire is unchanged and `comp-agentd`'s
  `--park-url` points at the park app instead of the daemon.
- **`holon:park@0.5.0` adds `sweep`**: the recovery pass (repair, commits, deadlines,
  re-announcement) and, for each resolution it announces, `resume::handle` against an HTTP sink
  (`park-sink-url`, `park-sink-token`). The gateway serves it on `POST /internal/pump`, and the
  app's `[triggers]` pump it every second.
- **Delivery is from the sweep, never inside the call that resolved it.** The sink may be the
  caller (`comp-agentd` waking its own approval); a delivery inside its call would be answered by
  itself. A resolution waits at most one pump plus `park-redeliver-secs` (default 1).
- **`reconciler/daemons/park` is deleted**, with `comp-park` and `comp-resume`. Replication is the
  host's keyvalue's (`--kv nats --kv-replicas 3`, ADR-0111), not a flag of its own.

## Consequences

- At least once is kept: a host killed mid-delivery leaves the ticket `ready`, and the next host
  on the same keyvalue delivers it again under the same `Idempotency-Key`
  (`reconciler/tests/park_app.rs`).
- `park-store` is 550 KB instead of a thin HTTP client: it carries the engine.
- A sink that is down is retried every `park-redeliver-secs`, not with `comp-resume`'s exponential
  backoff. At this scale that is a request a second; a backoff would need per-ticket state the pass
  does not keep today.
- `reconciler/tests/park_app.rs` (the old daemon end-to-end, ported) runs in CI; it needed a NATS
  before, and so never did.
