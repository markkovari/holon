# ADR-0002 — A tenant is a Kubernetes namespace

> **SUPERSEDED — superseded by [0021](0021-there-is-no-kubernetes.md).**
>
> Kept, not deleted: 2 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0021
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

The platform must give each tenant somewhere to put deployments, and the choice
of k8s object granularity decides what isolation we get for free versus what we
have to build.

`docs/PLATFORM.md` already decided the *compute* side: shared hostgroups, tenants
coexisting in one lattice, because per-tenant hostgroups is the cost model we're
avoiding. That decision is about where components *execute*. It says nothing
about how the k8s objects that describe them are grouped, and the two are
separable: many namespaces can schedule onto one shared hostgroup.

What the cluster gives us per namespace, at no build cost: `ResourceQuota`,
`LimitRange`, `NetworkPolicy` scoping, RBAC bindings, and deletion as a single
cascading operation. What it does not give us per *object name prefix*: any of
those.

The counter-pressure is real. Namespaces are not free in the v2 operator model —
`examples/eshop/k8s/` and `examples/jobs/k8s/` each install a registry and a
`Service` per namespace, and the operator's reach across namespaces has to be
established rather than assumed.

## Decision

**One namespace per tenant.** A tenant's deployments are objects inside it:

```
ns/tenant-<slug>
  WorkloadDeployment/<deployment-name>
  Service/<deployment-name>            (when something exports wasi:http)
  ResourceQuota/tenant                 platform-authored, from the tenant's plan
  NetworkPolicy/default-deny-egress    platform-authored
  ServiceAccount/…                     no tenant-facing k8s credentials
```

The namespace name is derived by the platform from the tenant id and is never
tenant-supplied. Tenants have **no k8s API access at all** — not a scoped RBAC
role, not a kubeconfig. Their only interface is the platform API (ADR-0003), so
the namespace is an implementation boundary, not a user-facing one.

Deletion of a tenant is deletion of the namespace, and that is the required
teardown path: anything the platform creates for a tenant must live inside the
tenant's namespace so that it dies with it.

## Consequences

- `ResourceQuota` becomes the enforcement point for the coarse limits (object
  counts, and CPU/memory for the docker lane), while `poolSize` /
  `maxInvocations` on the workload remain the wasm-side budgets. Two mechanisms,
  different granularity — ADR-0008 owns which is authoritative for what.
- `NetworkPolicy` default-deny-egress per namespace becomes the outer ring of
  egress control, with the workload's `localResources.allowedHosts` as the inner
  one. Belt and braces, and the belt is cluster-enforced.
- **Per-namespace plumbing must be automated before the second tenant exists.**
  Whatever the registry and ingress need per namespace is now the platform's job
  to create at tenant-creation time. This is the main cost of the decision and it
  falls due immediately.
- Cross-tenant sharing of a *running* service is now impossible by default, which
  is the intent. Sharing happens at the artifact level (ADR-0007), not by calling
  someone else's workload.
- The operator must be able to reconcile workloads in namespaces created after it
  was installed. **This is unverified and is the first thing to test** — if the
  installed chart is namespace-scoped, either it moves to cluster-scope or this
  ADR is wrong and gets superseded.

## Alternatives

- **One shared namespace with tenant-prefixed names.** Rejected: isolation would
  rest entirely on the platform stamping every field correctly, with nothing
  underneath it. A single missed `allowedHosts` or bucket prefix becomes a
  cross-tenant breach with no second line of defence, and the adversarial exit
  test in `docs/PLATFORM.md` phase 2 is the product — it should be passing against
  cluster mechanisms, not against our own diligence.
- **A namespace per deployment.** Rejected for now: the blast radius is nicer,
  but it multiplies the per-namespace plumbing cost by deployments rather than
  tenants, and quotas want to be per-payer. Worth revisiting if a tenant ever
  needs two deployments that must not see each other.
- **Virtual clusters (vcluster/Capsule).** Rejected as premature: it buys tenant
  self-service against a real k8s API, which we explicitly do not want (tenants
  get the platform API, not kubectl), at the cost of a whole new control plane to
  operate.
