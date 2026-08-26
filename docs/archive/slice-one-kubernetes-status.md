# Slice one, on Kubernetes: status and open risks (archived)

> **Archived 2026-08-26.** These two tables were the tail of `docs/adr/README.md`
> for a long time, and every row in them is about a world that no longer runs:
> Kubernetes namespaces, an `applier/` directory that is gone from the tree,
> rendered host pods, `NetworkPolicy`, wash 2.5.2, PVCs. The decisions that ended
> it are [ADR-0021](../adr/0021-there-is-no-kubernetes.md) (there is no Kubernetes)
> and [ADR-0022](../adr/0022-desired-state-is-a-manifest.md) (the reconciler pulls).
>
> They are kept because several rows record a *measurement* or a *bug found*, and
> ADR-0001's rule is supersede rather than delete. For what is true now, read
> [`docs/CURRENT.md`](../CURRENT.md). Nothing here is maintained.

## Implementation status (slice 1, ADR-0011)

| piece | where | state |
|---|---|---|
| renderer (`(graph, strategy, tenant, plan) → manifests`) | `components/platform-domain/src/render.rs` | **done** — pure, 17 unit tests |
| control plane (accounts, catalog, deployments, revisions) | `components/platform-domain/src/lib.rs` | **done** |
| applier (SSA + validation + re-apply loop) | `applier/` | **done** — 7 unit tests, validate-only mode needs no cluster |
| both strategies, planner-validated | ADR-0005 | **done, both proven live** (ADR-0018) — `fused` serves; `linked` wires 4 components in-process, no edges in the manifest |
| digest pinning enforced | ADR-0006 | **done** — a save with no digest is a 409 |
| isolation stamp (namespace, egress, blobstore containers) | ADR-0008 | **done** — and now per-app rather than per-tenant (ADR-0014) |
| a host per application (private data NATS, own engine, own endpoint) | ADR-0014 | **done and measured on a cluster** (ADR-0015) — a rendered host pod registers, a workload pins to it, and an app on its own bus cannot read another's buckets |
| image allow-list on the applier | ADR-0014 | **done** — a `Deployment` may only run the platform's two pinned images, and no host namespaces, privilege, hostPath or service account |
| delete an app (prune + orphan host reaping) | ADR-0016 | **done** — `DELETE /api/deployments/{id}?confirm=<app>`, label-scoped prune, and a sweep that reaps only what has neither a revision nor a live pod |
| drift correction (ADR-0004's re-apply) | `applier/` | **proven live** (ADR-0018) — Service deleted and host scaled to 0, both restored next pass, and the app's data survived the pod |
| e2e | `examples/platform/tests/platform.rs` | **done** — no cluster required |
| registry push (the digest source) | ADR-0017 | **done and proven against a real registry** — the applier pushes from the reconcile loop; upload → `deployable: true`, manifest resolves by the pinned digest |
| the registry itself | `examples/platform/k8s/registry.yaml` | **applied and in use** — 20Gi PVC, no NodePort; it served the live run (ADR-0018) |
| the whole path, live | ADR-0018 | **done** — upload → push → deploy → **serving HTTP with its own keyvalue**, plus a real delete. Found 3 bugs no review would have |
| `public` visibility | ADR-0007 | refused with `501` until signing exists |
| tenant secrets | ADR-0010 | refused until `secretFrom` is proven |
| studio canvas as the editor | ADR-0011 item 9 | not wired — the API is what exists |
| a second tenant, apps that touch keyvalue | ADR-0008 gate | **blocked** — adversarial test run on a cluster and FAILED (ADR-0012); enforced in code |
| a second tenant, apps that do not | ADR-0013 | **open** — HTTP/config/blobstore-only graphs are host-partitioned, so the gate does not apply to them |
| dedicated host per tenant (the escape hatch, and the tier) | ADR-0013 | **available** — `grant-shared-state=true` plus `template.spec.environment`, no catalog change needed |
| tenant config (`localResources.config`) | ADR-0010 | not wired — a deployed app cannot be configured yet, so `mesh` on the cluster answers `no route configured` |
| namespace scaffolding applied | ADR-0002 | **done** — it rides along with every save, because the app's host pod needs it (ADR-0014) |

Run it: `just host-platform` and `just e2e-platform`. There was a
`host-platform-live` recipe that actually applied against a cluster; it went with
the Kubernetes lane, so the applier no longer has a live half to be the
validate-only counterpart of.

## Open risks these ADRs name rather than solve

- ~~The keyvalue `buckets:` allow-list has never been exercised~~ → **tested on a
  real cluster, and it does not isolate.** Two tenants running the same app read the
  same records. The bucket is chosen by the guest's `store::open(name)`, not by
  manifest config, and every capability here hardcodes `"default"`. See
  [ADR-0012](../adr/0012-keyvalue-isolation-needs-a-cooperative-component.md); the gate is
  now **resolved** by ADR-0014 rather than worked around: the interface is bindable
  again because each application runs on its own host, whose data plane
  (`--data-nats-url`) is a loopback NATS sidecar in the app's own pod. Nothing else
  can reach the bus, so there is no bucket to allow-list. ADR-0013's default-deny was
  the interim answer and is superseded.
- ~~Namespace `NetworkPolicy` is inert for shared-host workloads~~ → **fixed by
  ADR-0014**: the app's host pod runs in the tenant's own namespace, so the policy
  selects it. It now has to allow the host's own egress (control-plane NATS, registry)
  or the app never registers.
- ~~The operator may not reconcile namespaces created after install~~ → **it does.**
  It holds ClusterRoles and runs with `-allow-shared-hosts=true`, and
  `template.spec.environment` schedules a workload onto a host in another namespace.
  ADR-0002 survives contact.
- **`secretFrom` has never been exercised** (ADR-0010). Until it is, no tenant
  secrets.
- **`wasi:keyvalue` has no CAS**, so all RMW state is best-effort and `lock:mutex`
  is advisory (ADR-0008). A published consistency envelope, not a bug to fix.
- **Per-workload CPU isolation is weak** (fuel traps composed apps), so
  noisy-neighbour risk is metered, not prevented *within* an app (ADR-0008). Between
  apps it is now a pod boundary (ADR-0014).
- ~~A rendered host pod has never been run.~~ → **run and measured** (ADR-0015),
  which also found and fixed a probe bug in it.
- **`NetworkPolicy` is enforced by nothing on this cluster** — no CNI daemonset, no
  policy controller, and a pod in an unlabelled namespace reached the registry. Every
  policy this platform emits is therefore documentation here, and the control that
  actually holds is `allowedHosts`, enforced by the wasmCloud host in the runtime
  (ADR-0018). Do not count the policies as a layer until the cluster enforces them.
- **`hostInterfaces[].name` is documented but broken** on wash 2.5.2 — setting it
  stops the workload linking at all (`resource implementation is missing`). Worth an
  upstream issue; `components/kv-probe` is the repro.
- **A host's bucket namespace is flat and guest-addressable.** Any component can
  `open()` any name on its host, including one belonging to another app. This is why a
  naming scheme is not an isolation mechanism (ADR-0015).
- ~~Deleting an app leaves its `Host` object behind~~ → **ADR-0016**, which also
  supplied the delete path the platform turned out not to have at all. Both residual
  risks are now closed: a live host whose object is deleted **re-registers in ~5s with
  the same host ID** (measured, so a wrong reap is a flap not an outage, and the sweep
  additionally requires no live pod), and an app delete requires `?confirm=<app>`
  because it destroys the storage claim. Still open: whether a workload's readiness
  flaps during those seconds, and retention (a soft delete keeping the PVC for a
  window) rather than only a confirmation prompt.
- **Host plugins are not an authoring surface** — "plugin" in the v2 model means the
  host's built-ins, and nothing here has ever registered one (ADR-0005). Per-tenant
  KV backends wait on upstream #5051.

## The shape those decisions added up to

```
   browser ──▶ platform-domain (wasm)          ──http──▶ applier (native) ──▶ k8s API
               auth-guard · records · policy             SSA, namespace +
               quota · blob · wit:reflect                prefix validation
                     │                                          │
                     │ renderer: (graph, strategy,              ▼
                     │  tenant, plan) → manifests      ns/tenant-<slug>
                     │                                   WorkloadDeployment
                     ▼                                   Service · Quota · NetPol
               registry (OCI, digest-pinned)
```

