# ADR-0011 — Slice 1 is single-tenant, both strategies, one cluster

> **SUPERSEDED — superseded by [0025](0025-slice-one-on-the-lattice.md).**
>
> Kept, not deleted: 2 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0025
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

ADRs 0002–0010 describe a multi-tenant platform. ADR-0008 also sets a release gate
that forbids a second tenant until an adversarial isolation test passes, and the
one storage-isolation mechanism that gate depends on (`buckets:` on keyvalue) has
no working precedent in this repo.

Meanwhile the deploy path that does exist is: hand-run Justfile recipes, a
manually-installed helm chart whose version is cited three different ways in two
adjacent files, an ephemeral registry with no auth, and one live tag-drift bug
(`jobs.yaml` wants `:0.1.2`, the recipe pushes `:0.1.0`).

So the first slice has to be honest about which end it is building from.

## Decision

**Slice 1 ships the full deploy path for one tenant — you — with both strategies,
and no tenant-facing multi-tenancy.**

In scope:

1. **`applier`** (native, ADR-0003): SSA apply of a manifest set into a namespace,
   plus observed status back. Namespace/prefix validation from day one.
2. **`platform-domain`** (wasm, ADR-0009): sign-in, projects, deployments,
   revisions. Composed from `auth-guard` + `records:store` + `policy:guard` +
   `blob:store` + `wit:reflect`.
3. **Renderer** (pure, in `platform-domain`): `(graph, strategy, tenant, plan) →
   manifests`, with golden-file tests against `eshop.yaml` and `vet-domain-v2.yaml`
   because those encode operator behaviour learned the hard way.
4. **Both strategies** (ADR-0005), validated by `wit:reflect`'s planner — `fused`
   via server-side compose, `linked` via one workload with one `hostInterfaces`
   entry per interface.
5. **Registry, made durable**: PVC plus authentication (ADR-0006 makes this a
   prerequisite, not hardening), with push from the platform rather than a laptop.
6. **Digest pinning end to end** (ADR-0006), which retires the tag-drift class.
7. **Periodic re-apply** (ADR-0004) — the drift story, in slice 1 rather than later.
8. **The catalog**: upload → `wit:reflect` inspect → surface stored; the repo's own
   109 components published by the platform tenant as the built-ins.
9. **The studio canvas as the deployment editor** — it already emits all three
   forms and validates edges with `wac`'s subtype checker; it becomes the front end
   rather than a separate demo.

Explicitly **out** of slice 1, each with the reason:

- **A second tenant.** Gated on ADR-0008's adversarial test. The data model is
  multi-tenant from the start; the product is not.
- **`public` visibility** (ADR-0007). Requires signing; `private` + `org` ship
  first.
- **Tenant secrets** (ADR-0010). Requires proving `secretFrom` on a real workload.
- **The docker lane.** `docs/PLATFORM.md` phase 4.
- **Metering and billing.** `docs/PLATFORM.md` phase 3; usage events can be emitted
  early but nothing aggregates them.
- **Custom domains, ACME, ingress.** Slice 1 routes by the operator's `Host` header
  on `:9191` with a platform-owned wildcard, as eshop does.
- **The v1 OAM lane.** Not offered (ADR-0005).
- **Any automatic cluster deploy from this repo's CI.** Deploys stay explicit.

The exit test for slice 1, in one sentence: **sign in, build a graph on the canvas
from uploaded and built-in components, pick a strategy, hit Save, and get a URL
that serves it — then change one component, Save again, and watch the revision
roll.** Done twice, once per strategy.

## Consequences

- The first thing built is the least glamorous: a native applier and a renderer
  with golden tests. That is deliberate — they are where the correctness lives.
- Everything tenant-shaped (namespaces, bucket prefixes, quotas, policy rules) is
  *implemented* in slice 1 but exercised with one tenant. The adversarial test then
  flips a gate rather than requiring a redesign.
- Because the platform is composed from the catalog, slice 1 has a natural
  dogfood milestone that should be treated as the real finish line:
  **deploy `platform-domain` with the platform.**
- The helm chart, registry storage and the `k8s-eshop` push-before-registry
  ordering bug all get fixed as a side effect, since the platform automates what
  the recipes do by hand.
- If the operator turns out not to reconcile namespaces created after install
  (ADR-0002's open risk), slice 1 discovers it in week one, which is the right time.

## Alternatives

- **Multi-tenant from day one.** Rejected: it would ship the isolation stamp
  untested against a real adversary, and ADR-0008's gate exists precisely to stop
  that trade.
- **One strategy first, the other later.** Rejected by explicit product choice — the
  strategy choice is the differentiating feature, so it is in the first slice even
  though it doubles the render/debug surface.
- **CLI first, UI later** (`docs/PLATFORM.md` phase 1's `plat deploy`). Rejected: the
  canvas already exists and is the thing that makes the strategy choice legible. A
  CLI remains a good later addition over the same API.
