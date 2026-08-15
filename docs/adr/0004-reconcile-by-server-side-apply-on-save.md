# ADR-0004 — Reconcile by server-side apply on save

> **SUPERSEDED — superseded by [0022](0022-desired-state-is-a-manifest.md).**
>
> Kept, not deleted: 2 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0022
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

"On save/update there is an actual k8s deployment happening" is a product
requirement, not an implementation detail: the tenant is sitting in front of a
canvas and needs to know within seconds whether their change worked.

Three ways for desired state to reach the cluster were considered (see
Alternatives). The deciding factor is the feedback loop, with a secondary concern
that we should not operate more machinery than the problem needs while there is
exactly one cluster and no tenants.

Relevant existing evidence: every deploy in this repo today is a hand-run
`kubectl apply` from a Justfile recipe, and the failure modes we have actually hit
are ordering (`k8s-eshop` pushes images before applying the registry that stores
them) and tag drift (`jobs.yaml` references `:0.1.2` while the recipe pushes
`:0.1.0`, so `just k8s-jobs` cannot pull on a fresh registry). Both are
render-time mistakes that a generator removes by construction.

## Decision

On save, the platform **renders the full manifest set and server-side applies it**
through the applier (ADR-0003), then polls for observed state:

```
save → validate graph (wit:reflect)
     → resolve every component to a DIGEST (ADR-0006)
     → render manifests (WorkloadDeployment [+ Service] [+ quota/policy])
     → SSA apply, one field manager: "platform"
     → poll observed status → store on the deployment record → stream to the UI
```

Rules:

1. **Server-side apply with a fixed field manager**, so the platform owns exactly
   the fields it sets and a human's `kubectl edit` shows up as a conflict rather
   than being silently clobbered.
2. **A deployment record is the source of truth**, versioned. Every save writes a
   new revision with the rendered manifests attached. Rollback is re-applying an
   earlier revision's manifests — no separate rollback machinery.
3. **Deletes are explicit.** A component removed from the graph means the
   corresponding object is deleted by name, computed from the diff between
   revisions. Orphan-by-omission is a bug class we opt out of by never relying on
   pruning.
4. **Re-apply is periodic and idempotent.** A background pass re-applies the
   current revision of every deployment on an interval, which is our drift
   correction. It must be safe to run at any time; that property is a test.
5. **The image tag is never a moving target** — see ADR-0006. Renders pin digests,
   which is what makes re-apply idempotent and kills the tag-drift class of bug.

## Consequences

- **We own retries, backoff and drift**, which a controller would have given us.
  The periodic re-apply is the mitigation and it must exist in slice 1, not later.
- Status is *observed*, not authoritative: the UI shows what the applier last saw,
  with a timestamp, and says so. No pretending to be live.
- Two writers to one namespace (the platform and a human with kubectl) is a
  supported situation, resolved by SSA conflict rather than last-write-wins.
- The renderer becomes the highest-value unit-test target in the platform: graph
  plus strategy plus tenant plan in, manifests out, pure function. It should have
  golden-file tests against the manifests we already know work
  (`examples/eshop/k8s/eshop.yaml`, `vet-domain-v2.yaml`), because those encode
  operator behaviour we learned the hard way — one `hostInterfaces` entry per
  interface, selector-less Services, `Host`-header routing on `:9191`.
- If the re-apply loop or status polling becomes the bottleneck, ADR-0004 is the
  one to supersede: the escape hatch is a CRD plus controller, and nothing above
  this line prevents that migration because the renderer is already pure.

## Alternatives

- **Own CRD + controller.** The k8s-native answer, and it hands us reconciliation,
  status subresources and retries. Rejected for slice 1: it is a second operator
  to write, install and version against a `v1alpha1` API we have already seen
  churn (the `k8s-collapse` recipe exists because a rollout can leave two host
  ReplicaSets at 1 and split the lattice). We take the simpler thing while the
  blast radius is one cluster, and keep the pure renderer so this stays open.
- **GitOps: commit rendered manifests, let Flux/Argo apply.** Attractive — every
  deploy becomes an auditable diff, rollback is a revert, and no cluster
  credential lives in the API. Rejected as the primary path on latency: a
  commit-poll-apply cycle is tens of seconds to minutes, and the UI promises
  "save and it's deploying". Retained as a **later addition**: the renderer can
  emit to git *as well*, giving the audit trail without owning the apply.
- **`kubectl apply` shelling out from the applier.** Rejected: parsing CLI output
  for status is worse than a typed client, and it drags a binary into the
  container for no benefit.
