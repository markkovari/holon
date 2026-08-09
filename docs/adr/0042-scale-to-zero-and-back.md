# 0042 — Scale to zero, and back

Status: accepted. Completes ADR-0038, using ADR-0040.

## What was missing

ADR-0038 shipped `min: 0` that **parked** an app rather than scaling it to zero: no
replica, so no route, so the ingress answered 503 and nothing could bring it back. The
field carried a comment saying so, because a `min: 0` that silently strands traffic is
worse than one that refuses.

Two things made finishing it reasonable. ADR-0037 measured the start at 33 ms and
named the constraint — **activation must not go through the reconcile loop**, or a 3
second poll becomes the cold-start latency. ADR-0040 then cached the compiled artifact
and took a warm start to **0.43 ms**, which turns "hold a request while we start
something" from absurd into ordinary.

## The path

```
request for a host with no replicas
  -> ingress: single-flight per host
  -> "activate" command to the pseudo-node `reconciler`
  -> reconciler: plan() for that ONE app with 1 in flight
  -> start command to the chosen node, awaited
  -> reply carries {node, address}
  -> ingress proxies this very request there
```

Three decisions worth stating:

**The ingress asks; it does not decide.** It holds no platform credential and no
manifest (ADR-0026), and that stays true — it sends a hostname and receives an
address. Everything about *whether* the app may run, and *where*, is decided by the
reconciler.

**`plan()` decides placement, not a second scheduler.** The activation handler calls
the same pure function the loop calls, with a synthetic load of one request in flight
for that host — which is precisely the signal that would have made the next pass place
a replica. Placement rules, constraints, and the stateful-spread refusal all apply for
free, and there is no second code path to keep in step.

**The reply carries the address.** Waiting for the activated instance to appear in
inventory would put a heartbeat interval in front of a user's request; the reply lets
the ingress route immediately, and the normal refresh picks it up a beat later.

Modelled as a command to a pseudo-node named `reconciler`, so it reuses the existing
command bus, subject naming and reply plumbing without a fourth trait on the lattice.

## Measured

One node, one app, `min: 0, max: 3, target: 10`:

```
replicas while idle:               0
first request (cold):              HTTP 200 in 49 ms
second request (now warm):         HTTP 200 in  2 ms
replicas once the heartbeat lands: 1 (after 1s)
replicas after it goes idle again: 0 (after 5s)
the request itself woke it:        yes
```

**49 ms for the caller that pays for the wake-up**, then 2 ms. It parks itself again
five seconds after the traffic stops, which is the existing scale-down hysteresis
doing its job — nothing new was needed for the down direction.

The test polls for these states rather than asserting on a snapshot. The first version
read the replica count the instant the request returned, got `0`, and reported failure
on a working system: inventory is a heartbeat behind reality, and an assertion that
does not know that is measuring the heartbeat.

## What this does not do

- **Single-flight is per ingress.** Two ingresses will both activate the same cold
  app. Harmless — a start carries an absolute count and is idempotent (ADR-0022) — but
  it is two commands where one would do.
- **`serve` is a plain subscription, not a queue group**, so two reconcilers would
  both act on one activation. Same shape of harmless waste.
  `// ponytail:` make it a queue group when a second reconciler is real.
- **A failed activation is a 503 with the reason logged, not returned.** The caller
  learns "no replica is placed"; the operator learns why from the log. Returning the
  planner's reason to an anonymous caller would leak which apps exist.
- **Nothing pre-warms.** The first request after a fleet restart still pays the 35 ms
  cold compile rather than 0.43 ms, because the node's `.cwasm` cache is empty.
