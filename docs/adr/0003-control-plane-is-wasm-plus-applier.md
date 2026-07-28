# ADR-0003 — The control plane is a wasm app plus a small native applier

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

`PLATFORM.md` says the platform API is "itself a wasm app". That is the right
instinct — a platform for deploying wasm components that is not itself one is a
weaker argument — but it collides with a hard constraint at exactly one point: the
Kubernetes API server.

A wasm component reaches the network through `wasi:http/outgoing-handler`. On the
native host that is `wasmtime-wasi-http`, which validates TLS against webpki roots.
The API server presents a certificate signed by the **cluster CA**, which is not
in those roots. There is no `wasi:tls` trust-store knob to add it. The remaining
options from inside wasm are to disable verification or to not use TLS — both
unacceptable in the one code path that can create workloads in any namespace.

Two more things only a native process can do comfortably: read the projected
ServiceAccount token (a mounted file — the host preopens no directories, so a
component cannot read one) and watch resources for status.

## Decision

Split the control plane in two:

```
   browser ──▶ [ platform-domain (wasm) ]  ──http──▶ [ applier (native) ] ──▶ k8s API
                 auth-guard, records,                  kube-rs, SSA,
                 policy, quota, wit:reflect            no business logic
```

**`platform-domain`** is a wasm component exporting `wasi:http/incoming-handler`,
composed from the catalog the same way every other app here is: `auth-guard` for
accounts/sessions/RBAC, `records:store` for tenants/deployments, `policy:guard`
for ownership checks, `quota:meter` for limits, `blob:store` for uploads, and
`wit:reflect` for inspection, planning and composition. It owns every decision:
who may deploy what, which strategy, what the manifests should say.

**`applier`** is a native Rust binary with the cluster credential. Its entire API
is *"apply this manifest set for this tenant, and tell me what happened"*. It
holds no business logic, no database, and no user concept. It is deliberately
small enough to audit in one sitting, because it is the component with the
dangerous permission.

The trust boundary is the applier's HTTP surface, which therefore:

- listens only on localhost / a ClusterIP reachable from the platform pod;
- authenticates the caller (shared secret from a k8s Secret, not from `wasi:config`);
- **validates that every object it is asked to apply is namespaced into the
  tenant namespace named in the request**, and rejects the request otherwise. The
  applier does not trust `platform-domain` to have got the namespace right. This
  check is what keeps a bug in the wasm side from becoming a cross-tenant write.

## Consequences

- The platform is a real dogfood: `platform-domain` is a component in its own
  catalog, inspectable by `wit:reflect`, composable by the studio, deployable by
  itself once bootstrapped. That is the demo.
- One more artifact to build and ship, in a different language, with its own
  release cadence. The applier's RBAC is the platform's real security posture and
  gets reviewed as such.
- `platform-domain` cannot watch k8s. Status flows the other way: the applier
  reports what it observed, and the wasm side stores it (ADR-0004).
- The applier is a chokepoint for the manifest vocabulary. It should validate
  against the **verified** field set (`replicas`,
  `template.spec.kubernetes.service.name`,
  `hostInterfaces[].{namespace,package,interfaces,config}`,
  `components[].{name,image,poolSize,maxInvocations,localResources}`) and refuse
  fields we have used only once and do not trust yet — `hostSelector.hostgroup`
  appears only in a stale `REPLACE_ME` file, and `configFrom`/`secretFrom` appear
  only in a comment.
- If upstream ever ships a `wasi:tls` trust-store or the operator exposes a
  guest-friendly API, the applier can shrink or vanish. Nothing else changes,
  because it has no state.

## Alternatives

- **Fully native control plane.** Rejected: it works, it's simpler, and it throws
  away the one thing that makes this platform interesting to look at. The repo's
  whole argument is that apps of this shape are composable wasm; the platform is
  the strongest possible instance of that claim.
- **Pure wasm, talking to the API server directly.** Rejected: it requires either
  trusting an arbitrary CA or skipping verification in the code path that can
  create anything anywhere. A design that needs a TLS workaround at its most
  privileged point is the wrong design, however good the purity argument.
- **Platform writes manifests, a GitOps agent applies them.** Not rejected on
  merit — see ADR-0004, where it lost on feedback latency for an interactive UI.
  It remains the natural upgrade for an audit trail.
