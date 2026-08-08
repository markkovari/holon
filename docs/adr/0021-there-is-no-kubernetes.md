# ADR-0021 — There is no Kubernetes; nodes are a lattice you join

- **Status:** accepted
- **Date:** 2026-08-08
- **Supersedes:** [ADR-0002](0002-tenant-is-a-namespace.md) (a tenant is a namespace), [ADR-0003](0003-control-plane-is-wasm-plus-applier.md)'s applier half

## Context

Every deployment mechanism in this platform existed to satisfy Kubernetes. A tenant was
a namespace because namespaces were what we had to isolate with. An application owned a
pod (ADR-0014) because a pod was the only thing we could give a private NATS to. Orphaned
`Host` objects needed reaping (ADR-0016) because the operator wrote objects the platform
could not label.

None of that was ever the goal. The goal was density on hardware we own, and ADR-0019
priced what Kubernetes was costing us: 70 Mi per pod against 2.3 Mi per extra component,
with the multi-tenant version blocked outright.

## Decision

**Bare-metal nodes joined by Tailscale, one `comp-host` process per node, a NATS lattice
between them, and no Kubernetes anywhere.**

Adding capacity is: install the binary, join the tailnet, point it at the lattice. A node
publishes what it is running to a JetStream KV bucket; a reconciler reads that bucket and
issues start/stop commands on subjects. That is the pre-operator wasmCloud + wadm model,
which never needed Kubernetes either — we adopted the operator, not the idea.

## Consequences

- **A vanished node needs no code.** Its inventory key has a TTL, so it expires. This
  deletes ADR-0016's entire orphan-reaping apparatus: the `--env-prefix` fence, the
  cross-namespace reach into the operator's namespace, the two-signals-before-reaping
  rule, and `--no-reap`. Roughly 200 lines replaced by `max_age`. That is the single
  largest simplification the substrate change buys and it is worth naming.
- **A save can no longer half-succeed.** The platform stores desired state; nothing is
  pushed anywhere at save time (ADR-0022). `apply_failed` as a deployment status is gone.
- **A tenant is no longer a namespace.** It is a prefix on a store name and a scope in a
  host's linker — see ADR-0023, which is where the isolation claim actually gets made and
  where it gets weaker.
- **What we lose:** everything Kubernetes was doing for free that we now do not do at all
  — no `NetworkPolicy` (it was enforced by nothing on our cluster anyway), no
  `ResourceQuota`, no scheduler with bin-packing, no rolling updates, no cluster autoscaler.
  The reconciler's placement is spread/daemon/pinned with equality label matching and
  nothing else.
- The measured claims in ADR-0019 and ADR-0020 were taken on wasmCloud under Kubernetes.
  They are not invalidated, but they are no longer measurements of *this* system, and
  re-taking them on the lattice is unfinished work.
