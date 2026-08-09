# Architecture decisions

Numbered, dated, one decision each, superseded rather than edited. Format and rules
in [ADR-0001](0001-use-adrs.md).

`PLATFORM.md` remains the narrative plan and the phase order; these own the forks
inside it. Where they disagree, the ADR wins.

**[`../WHY.md`](../WHY.md) is the value proposition, with the measurements behind it.**
**Read [ADR-0019](0019-the-density-number.md) for the numbers themselves.** The
multi-tenant density bet PLATFORM.md was built on is falsified (ADR-0012, ADR-0014). What
survives, measured: **2.3 Mi per extra component inside a host against 70 Mi for a
component in its own pod, and 1.2 ms saved per network hop avoided** — and under load,
**identical throughput and CPU, 3.2× less memory, and a 36% better p99**
([ADR-0020](0020-the-density-number-under-load.md)). So the value here is
decomposing one app into many components, not packing many tenants onto one host — and a
single-component app should be a container, not a wasm workload.

| # | decision | status |
|---|---|---|
| [0001](0001-use-adrs.md) | Record architecture decisions as ADRs | accepted |
| [0002](0002-tenant-is-a-namespace.md) | A tenant is a Kubernetes namespace | superseded by [0021](0021-there-is-no-kubernetes.md) |
| [0003](0003-control-plane-is-wasm-plus-applier.md) | The control plane is a wasm app plus a small native applier | applier half superseded by [0022](0022-desired-state-is-a-manifest.md); the split itself stands |
| [0004](0004-reconcile-by-server-side-apply-on-save.md) | Reconcile by server-side apply on save | superseded by [0022](0022-desired-state-is-a-manifest.md) |
| [0005](0005-deployment-strategy-is-a-tenant-choice.md) | Deployment strategy is a tenant choice: fused or linked | accepted |
| [0006](0006-artifacts-are-digest-pinned-oci.md) | Artifacts are digest-pinned OCI; the WIT surface is the contract | accepted; durability + auth revised by [0017](0017-the-applier-pushes-and-the-registry-is-a-cache.md) |
| [0007](0007-component-visibility-and-sharing.md) | Component visibility: private, org, public — and what public costs | accepted |
| [0008](0008-isolation-is-stamped-never-authored.md) | Isolation is stamped by the platform, never authored by tenants | superseded by [0023](0023-isolation-is-a-linker-boundary.md); its release gate re-met in [0026](0026-the-adversarial-run.md) |
| [0009](0009-identity-reuses-auth-guard.md) | Sign-in reuses `auth-guard`; OIDC is a later swap | accepted |
| [0010](0010-config-and-secrets.md) | Config is `wasi:config`; secrets never enter a manifest | accepted |
| [0011](0011-slice-one-scope.md) | Slice 1 is single-tenant, both strategies, one cluster | superseded by [0025](0025-slice-one-on-the-lattice.md) |
| [0012](0012-keyvalue-isolation-needs-a-cooperative-component.md) | Per-tenant keyvalue isolation needs a cooperative component | accepted; **answered** by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0013](0013-unenforceable-capabilities-are-denied-by-omission.md) | A capability the host cannot partition is denied by omission | superseded by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0014](0014-an-application-owns-a-host.md) | An application owns a host | superseded by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0015](0015-a-bucket-name-is-not-a-boundary.md) | A bucket name is not a boundary, and `hostInterfaces[].name` does not work | generalised by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0016](0016-deleting-an-app-is-reconciled-not-remembered.md) | Deleting an app is reconciled, not remembered | reaping half superseded by [0021](0021-there-is-no-kubernetes.md) |
| [0017](0017-the-applier-pushes-and-the-registry-is-a-cache.md) | The applier pushes, and the registry is a cache | accepted; auth reasoning corrected by [0018](0018-the-platform-deploys-a-running-app.md) |
| [0018](0018-the-platform-deploys-a-running-app.md) | The platform deploys a running app, and what that took | accepted |
| [0019](0019-the-density-number.md) | The density number, measured: 2.3 Mi per component, 70 Mi per app | accepted (idle/cold-start figures) |
| [0020](0020-the-density-number-under-load.md) | The same density number, under load: free throughput, 3.2× memory, better tail | accepted |
| [0021](0021-there-is-no-kubernetes.md) | There is no Kubernetes; nodes are a lattice you join | accepted |
| [0022](0022-desired-state-is-a-manifest.md) | Desired state is a manifest; the reconciler pulls it | accepted |
| [0023](0023-isolation-is-a-linker-boundary.md) | Isolation is a linker boundary, not a process boundary | accepted; measured in [0026](0026-the-adversarial-run.md); its backend table corrected by [0027](0027-a-spread-app-needs-a-shared-store.md) |
| [0024](0024-artifacts-are-content-addressed.md) | An artifact is its digest, and the object store is a cache | accepted |
| [0025](0025-slice-one-on-the-lattice.md) | Slice one, on the lattice: two boxes, one killed node | accepted; its cross-node reasoning corrected by [0028](0028-cross-node-calls-are-wrpc.md) |
| [0026](0026-the-adversarial-run.md) | The adversarial run: contained, at 10.5k rps, in 56 MiB | accepted; **discharges 0023's measurement** |
| [0027](0027-a-spread-app-needs-a-shared-store.md) | A spread app needs a shared store, and the platform now refuses otherwise | accepted |
| [0028](0028-cross-node-calls-are-wrpc.md) | Cross-node calls are wRPC; the codec I designed should never have existed | accepted |
| [0029](0029-one-address-in-front-of-n-replicas.md) | One address in front of N replicas, and its table comes from inventory | accepted; its balancer replaced by [0030](0030-least-outstanding.md) |
| [0030](0030-least-outstanding.md) | Least-outstanding, because round robin collapsed on a real fleet | accepted |
| [0031](0031-an-org-owns-a-deployment.md) | An organisation owns a deployment, and a person can be in several | accepted |
| [0032](0032-cross-node-invocation-and-what-the-hop-costs.md) | Cross-node invocation works, and the hop is nearly free (~4%) | accepted |
| [0033](0033-two-orgs-under-load.md) | Two organisations under load: what the platform costs and whether it holds | accepted |
| [0034](0034-two-machines-one-fleet.md) | Two machines, one fleet: placement does not map tenants to computers | accepted |
| [0035](0035-losing-a-machine.md) | Losing a machine, measured through the failure | accepted |
| [0036](0036-open-loop-stress-and-a-correction.md) | Open-loop stress from a third machine, and a correction to 0033/0034 | accepted |
| [0037](0037-what-a-cold-start-costs.md) | What a cold start costs, and why scale-to-zero is affordable | accepted |
| [0038](0038-autoscaling-on-observed-concurrency.md) | Autoscaling on observed concurrency (min/max/target) | accepted |
| [0039](0039-comp-versus-wasmcloud.md) | comp vs wasmCloud 2.x, same component, both machines | accepted |
| [0040](0040-compiled-artifacts-are-cached.md) | Compiled artifacts are cached (81x faster starts) | accepted |
| [0041](0041-the-ingress-sheds-load.md) | The ingress sheds load instead of queueing without bound | accepted |
| [0042](0042-scale-to-zero-and-back.md) | Scale to zero, and back — a request activates a parked app | accepted |
| [0043](0043-placement-weighs-capacity.md) | Placement weighs capacity, not just instance count | accepted |
| [0044](0044-subjects-carry-a-version.md) | Subjects carry a version | accepted |
| [0045](0045-shedding-feeds-autoscaling.md) | Shedding feeds autoscaling — a refused request is unmet demand | accepted |
| [0046](0046-what-the-signal-cannot-say.md) | What the signal cannot say — wedged vs saturated, at-ceiling, and absent vs idle | accepted |
| [0047](0047-config-is-declared-and-checked.md) | Config is declared by the uploader and checked at save | accepted |
| [0048](0048-does-this-plug-fit.md) | Does this plug fit? — the real subtype check, and typed request bodies | accepted |
| [0049](0049-the-org-can-see-it.md) | The org can see it — ADR-0007's middle row, and a market endpoint | accepted |
| [0050](0050-secrets-by-reference.md) | Secrets, by reference — stored, validated, not yet readable at runtime | accepted |
| [0051](0051-the-secret-reader.md) | The secret reader — a key, a handle, and one explicit reveal | accepted |

## The shape these add up to

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

Run it: `just host-platform` (applier in validate-only — it builds no Kubernetes
client, so the default loop cannot touch a cluster), `just e2e-platform`,
`just host-platform-live` to actually apply.

## Open risks these ADRs name rather than solve

- ~~The keyvalue `buckets:` allow-list has never been exercised~~ → **tested on a
  real cluster, and it does not isolate.** Two tenants running the same app read the
  same records. The bucket is chosen by the guest's `store::open(name)`, not by
  manifest config, and every capability here hardcodes `"default"`. See
  [ADR-0012](0012-keyvalue-isolation-needs-a-cooperative-component.md); the gate is
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
