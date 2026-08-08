# ADR-0033 — Two organisations under load: what the platform costs and whether it holds

- **Status:** accepted
- **Date:** 2026-08-08
- **Exercises:** [ADR-0031](0031-an-org-owns-a-deployment.md)'s ownership model and [ADR-0023](0023-isolation-is-a-linker-boundary.md)'s boundary, together

## Why one run

Every measurement so far took one app on a quiet fleet. That is not what the platform is
for. This runs the shape it exists to serve — **two organisations, five members each, both
deploying, both under load** — and takes the control-plane cost, the data-plane throughput
and the isolation check from the *same* run, because measuring them apart is how you end up
with a throughput number from an idle box and a safety number from a quiet one.

Three nodes, one control plane, one ingress, one NATS. 20 s at 40 connections **per org**,
concurrently.

## Control plane

| | |
|---|---|
| register + login, 10 users | 532 ms total, **53 ms each** |
| create 2 orgs + 8 invites + 8 joins | 67 ms |
| component upload, 2 orgs | 26 ms |
| create + deploy 2 apps | 12.1 s |

53 ms per user is argon2 doing its job — it is a password hash and it is supposed to cost
something. The 12.1 s is not the platform being slow: a **fused** deployment composes on the
first save and needs one distribution pass before the composed artifact has a content
address, so the first `deploy` legitimately fails and the second succeeds (ADR-0028). Both
orgs deployed on attempt 2. It is a two-phase operation wearing a stopwatch, and the honest
figure is "one reconcile interval", not twelve seconds of work.

## Data plane, both orgs at once

| | rps | p50 | p99 | success |
|---|---|---|---|---|
| acme | 3,046 | 12.5 ms | 25.1 ms | 100% |
| globex | 3,013 | 12.6 ms | 25.9 ms | 100% |

**~6,060 rps across the pair, split within 1%.** No org starved the other, and the tail
stayed under 26 ms with 80 connections in flight through one ingress. Latency is higher than
ADR-0030's single-app figure (p50 1.4 ms) because every request now crosses the ingress and
every store operation is NATS rather than a local file — both deliberate, and both already
priced separately.

## Isolation, checked after the load rather than before

```
b-app-acme-shop     961 values
b-app-globex-shop   620 values

acme member reading globex's app      -> 404 not_found
acme member deploying into globex     -> 404 no organisation `globex` that you belong to
globex member reading their own app   -> serves it
```

Two buckets, named for the **org** rather than for whoever ran the deploy, still separate
after ~120,000 requests. The control confirms the refusal is scoped rather than blanket: the
same call from a member of the owning org works.

## Memory

```
node 1          68 MiB   (holds both orgs' instances)
node 2          10 MiB
node 3          10 MiB
control plane   32 MiB
```

An idle lattice node is **10 MiB**. The one actually serving both organisations is 68 MiB —
against ADR-0020's ~233 Mi for a single-app host pod under Kubernetes. Indicative rather
than like-for-like (different substrate, different app, different machine), but it points
the same way ADR-0019 did.

## One thing this found

Both orgs' fused artifacts hashed to the **same digest**, so the platform stored and
distributed one artifact and both organisations run it. That falls out of content addressing
(ADR-0024) with no dedup logic anywhere, and it is worth naming because it looks alarming
and is not: the artifact is shared, the *instances* are not, and the store each one opens is
named from its own org.

## What this does not show

- **One machine.** Three nodes in three processes on one laptop. Nothing here says anything
  about a node under memory pressure or a saturated NIC.
- **Two orgs is not many.** The interesting failure modes for multi-tenancy — noisy
  neighbours, quota exhaustion, one org's burst hurting another's tail — need tens, and
  arrive with per-org limits that do not exist yet (ADR-0031's missing org-level plan).
- **The load is one endpoint.** A rate limiter is read-modify-write against the store; an
  app with a different shape will land somewhere else entirely.
- **Nothing was adversarial.** ADR-0026 remains the isolation measurement; this only shows
  the boundary surviving ordinary concurrent use.
