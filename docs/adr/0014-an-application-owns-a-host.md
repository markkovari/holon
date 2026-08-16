# ADR-0014 — An application owns a host

> **SUPERSEDED — superseded by [0023](0023-isolation-is-a-linker-boundary.md).**
>
> Kept, not deleted: 4 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0023
- **Date:** 2026-07-28
- **Confirmed by:** [ADR-0015](0015-a-bucket-name-is-not-a-boundary.md)
- **Supersedes:** [ADR-0013](0013-unenforceable-capabilities-are-denied-by-omission.md), and the density bet in `docs/PLATFORM.md`
- **Revises:** ADR-0002 (the namespace is the outer ring, not the isolation unit), ADR-0008 (isolation is provisioned, not only stamped)

## Context

ADR-0012 measured a leak and ADR-0013 responded by *removing* capability: with a
shared host, `wasi:keyvalue` and `wasmcloud:messaging` were refused to tenant code
because the bucket and the subject are named by the guest and the host applies no
per-workload restriction.

That answer was defensible and wrong for the product. The requirement is not "a
tenant is isolated from another tenant" — it is:

> one user should be able to deploy multiple applications, and their apps should have
> separate endpoints (or pubsub receivers, or webhooks), so we need separated storage,
> compute, and interfaces + implementations.

Two things follow immediately. **The isolation unit is the application, not the
tenant** — two apps of one user must not share a keyvalue bucket any more than two
users must, and ADR-0008's per-tenant stamp was the wrong granularity even where it
worked. And **basic interfaces are the product**: a platform that cannot offer
keyvalue or messaging is not offering wasmCloud, it is offering a subset chosen by
what our shared host happened to be unable to partition.

The mechanism was in the host binary the whole time. `wash host --help`:

```
--scheduler-nats-url <URL>   NATS URL for Control Plane communications
--data-nats-url <URL>        NATS URL for Data Plane communications
```

**The control plane and the data plane are separate flags.** `wasi:keyvalue`,
`wasi:blobstore` and `wasmcloud:messaging` are backed by the *data* NATS. The
operator only needs the *scheduler* NATS. And a host is not a provisioning CRD — the
`Host` object has no `spec`, only `environment`/`hostId`/`hostname`/`httpPort`,
because it is a **registration** written by a host that has started. A host is
therefore an ordinary `Deployment` the platform can render, and its data plane is
wherever we point it.

## Decision

**Every deployment renders its own host**, and the workload is pinned to it.

Per application, in the tenant's namespace:

1. A `Deployment` — `wash host` with `--host-group`/`--environment` set to
   `<tenant>-<app>`, `--scheduler-nats-url` pointing at the platform's shared control
   plane, and **`--data-nats-url=nats://127.0.0.1:4222`**.
2. A **NATS sidecar** in that pod, `-js -sd /data -a 127.0.0.1`. Loopback-bound: it
   has no Service, no cluster-network surface, and no other client. It is a **native
   sidecar** (`initContainers` + `restartPolicy: Always`) with an `exec` startup probe
   (`nc -z 127.0.0.1 4222`), because native sidecars order *start*, not readiness, and
   the host would otherwise race the bus. The probe must be `exec` and not
   `tcpSocket`: kubelet dials probes at the pod IP, so a loopback-only bind is refused
   forever (ADR-0015).
3. A `PersistentVolumeClaim` the sidecar stores to, so a restart does not lose the
   app's records.
4. The `WorkloadDeployment` with `template.spec.environment: <tenant>-<app>`.

What that buys, in the requirement's own terms:

- **Storage** — the app's buckets and streams live in a bus nothing else can reach.
  Not a name the guest is trusted to choose: a different NATS.
- **Compute** — its own wasmtime engine and its own core-instance budget, so one
  app's `poolSize` cannot starve another's (the vet-clinic failure, structurally
  fixed rather than clamped).
- **Interfaces and implementations** — every family the operator binds is granted,
  and the implementation behind each is that host's own plugin instance.
- **Endpoints** — its own `:9191` and its own operator-managed Service, so separate
  endpoints, pubsub receivers and webhook targets per app come for free rather than
  by Host-header multiplexing.

Consequences for the decisions this replaces:

- **ADR-0013 is superseded.** `TENANT_GRANTABLE` and the save-time `409` are gone;
  the renderer grants everything in `OPERATOR_BOUND`.
- **ADR-0012's multi-tenant gate is lifted.** It existed because tenants shared a
  bus. A second tenant is now refused nothing.
- **ADR-0008's storage stamp changes granularity**, from `t-<tenant>` to
  `b-<tenant>-<app>`, and is now belt-and-braces rather than the boundary.
- **ADR-0002's NetworkPolicy stops being inert.** The finding in ADR-0012 was that a
  policy in the tenant's namespace selected nothing, because components ran on a
  shared host in *another* namespace. The app's host pod now runs in the tenant's
  namespace, so the policy applies to it — and therefore it must explicitly permit
  egress to the control-plane NATS and the registry, or the app never registers.
- **The namespace scaffolding is applied with every save.** It was dead code; it is
  now load-bearing, because the host pod needs the namespace and the policy to exist.
  Re-applying it is idempotent, so drift heals instead of needing a provisioning step.

And the new risk this creates, with its guard:

> **`Deployment` on the applier's allow-list means the platform can run images.**
> Every other allowed kind is declarative data. This one executes code, and the
> platform is a wasm component tenants send HTTP to. So the applier does not trust
> the renderer: it re-derives the only two permitted images from its own flags
> (`--host-image`, `--nats-image`) and refuses `hostNetwork`, `hostPID`, `hostIPC`,
> `serviceAccountName`, `hostPath` volumes, `privileged` and
> `allowPrivilegeEscalation`. A service account would hand the pod a Kubernetes
> token, which is the one thing ADR-0003 exists to keep away from tenant-reachable
> code.

## Consequences

- **Density is now one pod per application** (two containers, one PVC), where
  `docs/PLATFORM.md` bet on many apps per hostgroup. That bet is retired: it was priced
  against a bucket stamp that ADR-0012 proved does not exist. What remains of the
  wasm density win is *within* an app — many components, one host, in-process links —
  which is where `linked` already lives.
- **Cold start is worse.** A new app waits for a pod, an image pull and a JetStream
  init instead of landing on a running host. The `oci-cache` volume is per-pod now, so
  the first pull is never warm. If this matters, the fix is a pre-warmed pool of hosts
  claimed on save, not a return to sharing.
- **Scale-out per app is capped at one host** by construction: JetStream on a
  ReadWriteOnce claim cannot tolerate two writers, which is why the Deployment uses
  `strategy: Recreate` and `replicas: 1`. `Plan.replicas` still scales the workload's
  instances on that host. Going past one host per app needs a real NATS cluster per
  app, which is a much larger object; that is the ceiling and it is deliberate.
- **The quota now counts what costs money** — pods, claims and storage, not just
  workload objects.
- **`grant-shared-state` and `allow-multi-tenant` are gone.** Both existed to make a
  shared host survivable. Deleting flags is the best outcome available for a design
  whose default was dangerous.
- ~~What is still unproven: that a rendered host pod registers with the operator and
  serves.~~ **Measured on the cluster — see [ADR-0015](0015-a-bucket-name-is-not-a-boundary.md).**
  A host pod rendered as a plain Deployment registers itself, a workload pinned by
  `environment` schedules onto exactly that host, `wasi:keyvalue` binds against a
  loopback NATS sidecar, and an app on its own bus cannot see a bucket an app on
  another bus wrote — the same read that succeeded on a shared host. One bug in this
  ADR's manifest was found and fixed in the process: the sidecar's `startupProbe` must
  be `exec`, not `tcpSocket`, because kubelet dials probes at the pod IP and a
  loopback-only bind is refused forever.

## Alternatives

- **Keep the shared host and deny keyvalue** (ADR-0013). Rejected by the requirement:
  the basic interfaces are the product, and per-app separation cannot be built on a
  shared bus at all.
- **One host per tenant, apps share it.** Cheaper, and wrong at exactly the point
  this ADR is about: two apps of one user would share a bucket namespace, so
  `records:store`'s hardcoded `"default"` would collide between a user's own apps.
- **Cooperative buckets** (ADR-0012 part 3): every capability reads its bucket name
  from `wasi:config`. Still rejected, and now clearly unnecessary — it changes ~10
  catalog components to reach a boundary a guest can ignore, while this reaches a
  stronger one with no catalog change at all.
- **A NATS Deployment + Service per app instead of a sidecar.** Same isolation, more
  objects, and a network-reachable bus that then needs a NetworkPolicy to defend.
  Loopback needs no defending.
- **Per-tenant NATS accounts on one shared bus.** The genuinely denser answer, and
  the one to revisit if pod-per-app hurts: accounts isolate subjects and KV stores
  inside one server. Rejected for now because `wash host` takes no NATS credential
  flags (only TLS), so it would mean cert-based account mapping — a new operational
  surface, against one flag that already works.
- **Wait for upstream multi-backend `hostInterfaces` (#5051).** It would make a
  shared host partition storage properly and restore density. Still worth having;
  it is no longer blocking anything.
