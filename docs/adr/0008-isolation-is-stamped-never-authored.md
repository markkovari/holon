# ADR-0008 — Isolation is stamped by the platform, never authored by tenants

> **SUPERSEDED — superseded by [0023](0023-isolation-is-a-linker-boundary.md); its release gate re-met in [0026](0026-the-adversarial-run.md).**
>
> Kept, not deleted: 4 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0023
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

Tenants share hostgroups (`docs/PLATFORM.md`'s density bet, unchanged by ADR-0002's
namespace split). wasm gives compute isolation for free. Data and network
isolation are ours to add, and the mechanisms exist in the operator's vocabulary:

- `hostInterfaces[].config` — e.g. `buckets:` to allow-list blobstore containers;
- `components[].localResources.allowedHosts` — fail-closed egress allow-list;
- `components[].{poolSize,maxInvocations}` — concurrency and warm-instance budgets;
- namespace-level `ResourceQuota` / `NetworkPolicy` (ADR-0002).

Two things must be said plainly, because the plan in `docs/PLATFORM.md` reads more
finished than the evidence supports:

1. **The `buckets:` allow-list has only ever been exercised for blobstore**, once,
   in `bench-suite-v2.yaml`. Every deployed workload in this repo shares the
   keyvalue bucket `"default"` — eshop's own manifest says the workloads "share
   nothing but the host's NATS-backed `wasi:keyvalue` (bucket `default`)". The KV
   isolation primitive the multi-tenant story depends on is **unproven**.
2. **`wasi:keyvalue` has no CAS**, so all read-modify-write state is
   single-writer/best-effort, and `lock:mutex` is advisory only. That is a
   correctness envelope tenants must be told about, not a detail.

## Decision

**Tenants never author isolation fields. The platform stamps them, and the applier
verifies them.**

Concretely, on every render:

- **Storage.** Every keyvalue and blobstore `hostInterfaces` entry is stamped with
  a bucket/container name derived from the tenant id, plus the allow-list config
  restricting the workload to it. Tenants get a logical namespace, never a bucket
  name. A deployment that would land on a bucket outside its tenant's prefix is a
  render bug that the applier rejects.
- **Egress.** Default-deny. `allowedHosts` is generated from the tenant's approved
  destination list, never taken from the deployment spec verbatim. Note the
  observed quirk: entries are needed **both bare and port-qualified**
  (`host` and `host:9006`), per `examples/jobs/k8s/jobs.yaml` — the generator emits
  both forms so nobody debugs this twice.
- **Resources.** `poolSize` and `maxInvocations` come from the tenant's plan, with
  a ceiling. `poolSize × core-modules-per-instance` must stay well under the host
  engine's concurrent-core-instance cap — the vet-clinic learned this at
  `poolSize: 48` (1344 cores starved the host); it settled at 16. The renderer
  needs the component's module count (from `wit:reflect`) to compute a safe pool,
  which is a second reason the surface is load-bearing (ADR-0006).
- **The applier re-checks** namespace, bucket prefix and the presence of an egress
  allow-list before applying (ADR-0003). Two independent checks, because the first
  one is in the component that tenants can reach.

And the gate on multi-tenancy itself:

> **No second tenant until the adversarial test passes.** Two tenants on one
> hostgroup; tenant A's component provably cannot read B's buckets, call B's
> services, or exceed its egress allow-list. `docs/PLATFORM.md` already names this the
> product. This ADR makes it a **release gate**: until it passes, the platform runs
> single-tenant (you are the tenant), regardless of what the UI can express.

The keyvalue bucket allow-list is the first thing that test must prove, since it
is the one mechanism above with no working precedent.

## Consequences

- The isolation stamp is a pure function — `(tenant, plan, graph, surfaces) →
  isolation fields` — and it gets property tests before it gets a UI. It is the
  single highest-consequence function in the platform.
- **A tenant cannot bring their own KV backend** while a v2 host serves one
  environment. Per-tenant *backends* wait for upstream #5051; per-tenant *buckets*
  are what we ship. Tenants needing a private datastore use their own external
  database over egress (the `docs/PLATFORM.md` phase-4 tunnel story), not our KV.
- Tenants must be shown the consistency envelope: no CAS, so RMW is best-effort
  and advisory locking is advisory. Catalog components should carry this per
  component (`records:store`'s revision check is best-effort under contention —
  the `gate` app documents exactly this and where it breaks).
- Per-workload CPU is still weakly isolated (fuel traps composed apps), so
  noisy-neighbour risk is metered and alerted, not prevented. A "dedicated
  hostgroup" tier is the honest escape hatch and also the premium SKU.
- Secrets must not travel in `localResources.config`, which is plaintext in the
  manifest — `vet-domain-v2.yaml` currently ships a `master-key` that way. See
  ADR-0010.

## Alternatives

- **Let tenants write raw `hostInterfaces` / `allowedHosts`.** Rejected: it is a
  cross-tenant read away from a breach, and the fields are exactly the ones a
  hostile tenant would target first.
- **A mutating admission webhook instead of render-time stamping.** The
  k8s-idiomatic version, and it would catch objects created by any path. Rejected
  for slice 1 because with ADR-0004 the platform is the only writer, so a webhook
  duplicates the check while adding a cluster-wide failure mode (a broken webhook
  can block all applies). Revisit if anything else ever writes workloads.
- **Namespace-per-tenant alone as the isolation story.** Rejected: it isolates k8s
  objects, not the shared host's KV buckets or egress. The workload-level stamp is
  what actually separates tenants at runtime; the namespace is the outer ring.
- **Wait for #5051 before multi-tenancy.** Rejected: bucket prefixing is sufficient
  if proven, and the adversarial test is what tells us whether it is. Waiting on
  upstream would stall the product on someone else's roadmap.
