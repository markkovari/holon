# 0037 — What a cold start costs, and why scale-to-zero is affordable

Status: accepted. Decides the design of autoscaling (min/max, and whether true zero
needs an activation path).

## The reframing that comes first

A "replica" here is not a running instance. `host/src/main.rs` instantiates the
guest **per request**:

```rust
let proxy = pre.instantiate_async(&mut store).await?;
```

Between requests nothing runs — no process, no thread, no instance. A replica is a
*registration*: a `Scope`, a compiled `Component`, a `ProxyPre`, and a route-table
entry. So an idle app already costs zero CPU. Scale-to-zero on this platform buys
back **compiled-module memory and a routing entry**, not a runtime.

That means the interesting question is not "can we avoid keeping something running"
— we already do — but "what does it cost to put a registration back".

## The measurement

One node, one component (0.4 MB, `gate-domain` fused), start/stop driven directly
over the command bus with the reconciler stopped so it cannot re-place underneath
the run. The host reports its own phase timings, because timing from the far side of
a NATS round trip would fold in the `nats` CLI's ~100 ms startup. 11 starts, every
other one with the artifact cache evicted first.

| phase | median | min | max |
|---|---|---|---|
| **total** | **33 ms** | 30 | 45 |
| fetch (object store) | 1 ms | 0 | 3 |
| compile | **31 ms** | 30 | 42 |
| link + `instantiate_pre` | 0 ms | 0 | 0 |

**Compile is 94% of a cold start.** Eviction was verified to hit the real cache
directory (`state_dir/artifacts`), so the 1 ms fetch is a genuine object-store pull —
of a small component over loopback, which is the flattering case and should not be
read as "distribution is free" on a real network.

And the ack's promise holds: the first request after a start ack was served in
**2 ms**, measured rather than asserted, since the ack is deliberately sent after
`instantiate_pre` so that "started" means "will serve".

## What this decides

**33 ms is well inside an activation budget.** A request that arrives at a
scaled-to-zero app can trigger a start and be held, and the caller sees something in
the tens of milliseconds — not the seconds a container platform pays. True
scale-to-zero is affordable here, and the activation path is worth building.

The constraint is *not* the start; it is that activation must bypass the reconciler.
A 3-second reconcile interval as cold-start latency would make the number above
irrelevant — 33 ms of work behind a 3 s poll.

## The optimisation this exposes, which is not built

`fetch_artifact` caches the raw `.wasm`, and `start` calls
`Component::from_file` — so **every start recompiles the component**. The plan called
for caching `.cwasm` and using `deserialize_file`; it was never built. Doing it would
take the dominant 31 ms toward the ~1 ms range, and it pays out three times over:
cold starts, every re-placement after a node dies (ADR-0035's failover), and node
reboots.

`// ponytail:` `deserialize_file` is `unsafe` — it trusts the bytes. Host-written,
host-private directory only, keyed by the artifact digest, never anything off the
wire. That constraint is the whole reason it is safe to do at all.

## What is not proven

- One component, 0.4 MB, on an M-series Mac. Compile time scales with module size;
  a 5 MB component should be expected in the hundreds of milliseconds, unmeasured.
- The pull was loopback. malna over the LAN would show a real fetch cost.
- Nothing here measures *concurrent* starts. A burst of activations compiles in
  parallel on the same node, against the pooling budget — the starvation case in
  ADR-0008, now arriving at runtime.
