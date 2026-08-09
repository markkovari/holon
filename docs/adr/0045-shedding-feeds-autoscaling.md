# 0045 — Shedding feeds autoscaling

Status: accepted. Closes the gap between ADR-0038 and ADR-0041.

## The two features were fighting

ADR-0038 scales on **observed concurrency**, published by the ingress. ADR-0041 gave
the ingress a bound and made it **refuse** past it.

A refused request never becomes in-flight. So concurrency understates demand exactly
when demand is highest: the ingress turns traffic away at the door while the
reconciler sees a calm app carrying eight requests and declines to grow it. Each
feature is correct alone and together they deadlock — the app stays small *because*
it is overloaded.

## The change

The ingress counts refusals per host and publishes them alongside in-flight; the
reconciler adds them:

```
demand = inflight + refused-since-last-publish
```

The counter is **taken and reset** each publish, so it measures the interval rather
than the lifetime — a cumulative count would keep demanding replicas long after the
pressure was gone.

Counting a refusal as one unit of unmet concurrency is deliberately crude, and the
crudeness is the point. It is not a measurement of how much load was turned away and
does not need to be: its job is to push `desired` upward while refusals continue, and
`max` is where it stops. **An app that is shedding should go to its ceiling — that is
what the ceiling is for.**

## Measured

Four nodes, `min 1 / max 4 / target 20`, and `--max-inflight 8` so load hits the bound
rather than the fleet's real capacity. Identical bench, identical load; the only
difference is whether the reconciler counts refusals.

| | sheds ignored | sheds counted |
|---|---|---|
| replicas | 1 → 1 | **1 → 4 → 1** |
| requests served | 104 054 | **183 092** |
| time to reach max | never | ~6s |

**76% more requests served**, from the same fleet under the same load, because the
platform was allowed to notice it was refusing traffic. It returns to `min` when the
load stops, on the existing scale-down cooldown.

## The bug this found on the way

The first three runs showed the signal present in the bucket, the manifest carrying
its `scale` block, and the fleet stubbornly at one replica. The reconciler was
connecting to the load bucket with `.ok()` — so a failure to open it produced
`None`, which reads as "no samples", which reads as "no traffic". Autoscaling looked
broken when it had simply never been connected.

That path now logs. An optional input is still allowed to be absent; it is not allowed
to be absent *quietly*, because the observable behaviour of "no load signal" and "no
load" are identical and only one of them is a fault.

(The actual cause that day was a stale binary — `cargo test` after the change, not
`cargo build`, so the bench ran a reconciler without it. Third time this session that
a stale binary has produced a convincing wrong result: ADR-0034 shipped one to malna,
ADR-0044's rehearsal mixed a versioned host with an unversioned reconciler.)

## What this does not do

- **No distinction between kinds of 5xx.** A refusal is counted where it is raised, so
  a backend's own 503 is not mistaken for shedding — but nothing separates "shedding
  because saturated" from "shedding because every replica is dead", and the second
  should arguably not ask for more replicas of something that keeps dying.
- **It cannot scale past `max`,** which is correct, and it also cannot tell you that
  `max` is too low. A fleet pinned at `max` while still shedding is the signal an
  operator wants and nothing surfaces it.
- **No rate limiting of the growth.** Demand jumps to the ceiling in one pass. With
  `max` small that is right; with a large `max` it is a stampede waiting to be
  measured.
