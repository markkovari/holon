# ADR-0016 — Deleting an app is reconciled, not remembered

- **Status:** accepted
- **Date:** 2026-07-28
- **Resolves:** the orphaned-`Host` risk left open by [ADR-0015](0015-a-bucket-name-is-not-a-boundary.md)

## Context

ADR-0015 recorded that a deleted host leaves its `Host` object behind — both
experiment hosts stayed registered after their pods and namespace were gone, and had
to be deleted by hand. It filed that as **open**, which was too passive: the reason
it was awkward is that `Host` lives in the operator's namespace and cannot be
labelled by us, not that it was unsolvable.

Looking properly turned up something worse. **There was no delete path at all.** No
endpoint on the platform, no delete in the applier. Deleting an app was not a
half-finished feature; it was absent, and every app ever created left a
`WorkloadDeployment`, a host `Deployment`, a `PersistentVolumeClaim`, a `Service` and
a `Host` behind permanently.

Three facts shape the fix:

1. A `Host` is **self-registered**. The host pod heartbeats on the scheduler NATS and
   the operator writes the object, in the **operator's** namespace, under a
   **generated name** (`spotless-dock-5472`). The platform can neither label it nor
   predict its name. The only handle is `spec.environment`, which the platform *does*
   control, because it puts it on the host pod's command line.
2. Nothing else reaps it. A stopped host's object survives indefinitely (measured).
3. The applier already has a reconcile loop (ADR-0004) that polls the platform for
   current revisions, because a wasm component has no background thread.

## Decision

**Deletion is a reconciliation, not a remembered list of names.**

- **The platform exposes `DELETE /api/deployments/{id}`.** It prunes the footprint
  *first*, then deletes its own records. That order is the whole safety argument: the
  records are what the reconciler reads to decide what is live, so dropping them
  before the cluster is clean would strand a running app that nothing would ever
  collect. A failed prune is a `502` and the deployment stays.
- **The applier exposes `POST /prune {namespace, env}`** and deletes by **label
  selector**, not by name: every object the renderer emits now carries
  `platform.comp/env`. The platform sends a selector because a list of names is
  something it could get subtly wrong, and objects missing from that list would leak
  forever.
- **The `Host` is matched by `spec.environment`**, in `--operator-namespace`. This is
  the only place the applier reaches outside a tenant namespace, and it is delete-only
  — it never creates or patches there.
- **The environment carries a reserved `app-` prefix** (`app-<tenant>-<app>`). This is
  the ownership marker that makes the reach safe: the reaper only ever *considers* a
  `Host` whose environment starts with the prefix, so the chart's own hosts (`jobs`,
  `eshop`, `default`) are invisible to it by construction rather than by carefulness.
- **The reconcile loop reaps orphans**, not just what it was told to delete. Live
  environments come from the revisions it already polls; every platform-owned `Host`
  outside that set is an orphan, however it got that way — a delete that half-finished,
  a crashed applier, a namespace someone removed by hand. A reconciler that only
  cleans up when asked never converges after the one failure that matters.
- **An orphan needs two things missing, not one: no revision AND no host pod.** The
  live set comes from the platform, so a `Host` whose pod is still running but whose
  revision the platform has lost would look like an orphan — and reaping it would be
  the wrong answer to a different bug (the platform forgetting a running app). The
  second half is a positive liveness check against `Deployment`s labelled
  `platform.comp/env`, which also means a sweep cannot race a host that is starting.
  When the two disagree the applier logs it and reaps nothing, naming the real bug.
- **Deleting an app requires naming it**: `?confirm=<app>`, or `428 Precondition
  Required`. The operation destroys the app's storage claim and nothing here can undo
  that, so an accidental or replayed `DELETE` must not be able to succeed.
- **A failed poll changes nothing.** This matters far more for reaping than for
  applying: treating "the platform did not answer" as "there are no apps" would delete
  every platform-owned host on the cluster. The loop already `continue`d on a failed
  poll; that behaviour is now load-bearing and commented as such.

What deletion does **not** touch: the tenant's namespace, `ResourceQuota` and
`NetworkPolicy`. Those belong to the tenant, not to one app, and the tenant's other
apps are still running in there. A tenant's own teardown is a separate operation and
is not in this ADR.

## Consequences

- **The PVC goes with the app**, so deleting an app destroys its data. That is the
  honest reading of "delete", and the `?confirm=<app>` token is the guard rather than
  the answer — the UI must still say what is about to be destroyed. If a grace period is wanted, the place for it is a `deleted_at` on the
  record with the prune deferred — not a PVC left behind, which would be storage
  nobody is accounting for.
- **Deleting a live host's `Host` object is self-healing — measured.** A running host
  whose object was deleted re-registered in **~5 seconds, with the same name and the
  same host ID**, confirming the object is a projection of the live host rather than
  its identity. So a reap that fires wrongly costs a flap, not an outage. (What is
  still untested is whether a workload's readiness flaps during those seconds; the
  probe host ran no workloads.)
- **Orphan reaping is a cluster-wide sweep** on an interval, so its cost grows with
  the number of `Host` objects, not with the number of deletes. At this scale that is
  nothing; if it ever isn't, the fix is a field selector on the environment prefix.
- **`--no-reap` exists** for an operator who wants the sweep off, and every deletion
  is logged with the object and its environment. A process that deletes things on a
  timer should be easy to turn off and impossible to be quiet about.
- The `app-` prefix costs 4 characters of the 63-character DNS budget, which the
  53-character truncation in `env_for` already accounts for.

## Alternatives

- **Delete-driven only** (prune what the delete call names, no sweep). Rejected: it
  cannot recover from its own failure, and the failure it cannot recover from is
  exactly the one that leaves a host running that the platform has forgotten.
- **Prune by name list.** Rejected: the platform would have to remember every object
  it ever rendered for an app, and anything it forgot would be invisible forever. A
  label is derived from the same function that created the objects.
- **Label the `Host` object.** Not available — the operator creates it from a
  heartbeat, so there is nothing of ours on it. This is why the environment prefix
  exists.
- **Let the operator reap its own stale hosts.** Where this belongs, and worth an
  upstream issue alongside ADR-0015's. Rejected as the platform's answer because it is
  someone else's roadmap and the objects accumulate today.
- **Garbage-collect with owner references.** The idiomatic Kubernetes answer, and it
  would delete the footprint automatically when a parent object goes. Rejected for now
  because the `Host` has no owner we control and lives in another namespace (owner
  references cannot cross namespaces), so it would solve the easy half and leave the
  half this ADR is about.
