# ADR-0013 — A capability the host cannot partition is denied by omission

> **SUPERSEDED — superseded by [0023](0023-isolation-is-a-linker-boundary.md).**
>
> Kept, not deleted: 3 decisions still in force cite this one, and the
> record of how the platform got its shape is the point of keeping ADRs at all
> (ADR-0001). Nothing below is edited to look wiser than it was. For what is true
> now read [`../CURRENT.md`](../CURRENT.md); for what is in force read
> [the index](README.md).

- **Status:** superseded by ADR-0023
- **Date:** 2026-07-28
- **Supersedes:** —
- **Builds on:** [ADR-0012](0012-keyvalue-isolation-needs-a-cooperative-component.md)

## Context

ADR-0012 established that `wasi:keyvalue` does not isolate tenants: the bucket is
named by the guest's `store::open(name)`, every capability in this catalog hardcodes
`"default"`, and the `config: { bucket: … }` the renderer stamped was read by
nothing. It left the platform single-tenant and named three possible fixes without
choosing one.

The gap ADR-0012 left is that its enforcement was *negative knowledge*: we knew the
stamp did nothing, so we stopped emitting it and refused a second tenant in code. But
a `wasi:keyvalue` entry was still being rendered for tenant code — unstamped and
shared. The refusal sat at the tenant-count check, one config flag away from a
cross-tenant read.

The question was what mechanism could make the boundary real without waiting on a
catalog change. One experiment answered it. A workload was deployed whose component
imports `wasi:keyvalue/store` with **no matching `hostInterfaces` entry**:

```
component imports instance 'wasi:keyvalue/store@0.2.0-draft', but a matching
implementation was not found in the linker ... unbinding all plugins
```

The workload does not start. It does not start *degraded*, and it does not fall
through to a default backend. **Omission fails closed** — which makes the absence of
an entry an enforceable boundary, in a way its presence never was.

## Decision

**The renderer emits a `hostInterfaces` entry only for capabilities the host can
partition per workload. Everything else is denied by leaving it out.**

- **Grantable to tenant code on a shared host:** `wasi:http` (the operator routes by
  `Host` header, and egress is separately allow-listed per component),
  `wasi:config` (a per-component map the platform authors), `wasi:blobstore` (the
  `buckets:` allow-list is enforced by the host's plugin, not chosen by the guest —
  the one storage mechanism with a working precedent).
- **Denied:** `wasi:keyvalue` and `wasmcloud:messaging`. Both are host-provided,
  shared, and named by the guest. There is no field on either that restricts one
  workload to one slice.
- **The refusal is at save, not at deploy.** `platform-domain` returns `409` naming
  the offending interface (`wasi:keyvalue/store`) before rendering anything, so a
  tenant gets a readable error rather than a workload that fails to link. The
  fail-closed linker is the backstop, not the UX.
- **The escape hatch is a host of your own.** `grant-shared-state=true` lifts the
  denial, and it is only correct for a tenant whose workloads run on a host
  environment nobody else shares — where "shared state" is shared with itself. This
  is ADR-0012's deferred alternative, and it is the premium/dedicated tier. It needs
  **no catalog change**: `template.spec.environment` already schedules a workload onto
  a host in another namespace, which was proven on the cluster.
- ADR-0012's cooperative-bucket work (a capability reading its bucket name from
  `wasi:config`) stays **deferred**, and is now clearly the *second* choice: it
  touches every keyvalue-importing component in the catalog to buy what a dedicated
  host already gives. It becomes load-bearing only if the upstream item below lands,
  at which point the host does the partitioning and the catalog change is the cheap
  half.

The upstream ask, stated so it can be filed: **a keyvalue allow-list on
`hostInterfaces` mirroring blobstore's `buckets:`** — host-enforced, guest-opaque.
That is the single change that would move `wasi:keyvalue` from the denied list to the
grantable one and restore the density bet.

## Consequences

- **The catalog splits in two along a line nobody drew on purpose.** Any app whose
  graph touches `wasi:keyvalue` — which is most stateful ones, including `mesh` via
  `records:store` — is now deployable only on a dedicated host. Pure-compute and
  HTTP-shaped apps run on the shared host. The platform must say which side an app
  falls on *at save*, and it does.
- The e2e reflects this: the main flow runs with the grant (the deployment under test
  uses `records:store`), and a second test asserts the default-deny `409` on a graph
  that imports keyvalue without it. The denial is tested, not assumed.
- `grant-shared-state` is a **footgun with a safety on it**: setting it for a tenant
  on a shared host reintroduces exactly the cross-tenant read ADR-0012 measured. The
  name says "grant", the ADR says when, and the tenant-count gate still stands behind
  it. If this ever becomes a per-tenant field rather than a platform-wide config, it
  must be coupled to the environment assignment so the two cannot disagree.
- Density is now a per-app property rather than a platform-wide bet. That is a worse
  business position than PLATFORM.md assumed and a better one than ADR-0012 left us
  with, because it ships a real second tenant instead of waiting.
- The denial is a *default*, and defaults drift. The grantable list lives in one
  `const` in `render.rs` mirrored by one in `lib.rs`, both with tests. Adding an
  interface to it is the highest-consequence one-line change in the platform.

## Alternatives

- **Keep rendering a shared keyvalue entry and rely on the tenant-count gate.**
  Rejected: that is the state ADR-0012 left, and the boundary is a config flag rather
  than a mechanism. A capability tenant code can reach must be denied by the same
  thing that would stop an attacker, not by a counter.
- **Cooperative buckets first** (ADR-0012 part 3). Deferred as above: it changes ~10
  catalog components to reach a boundary that is still cooperative — a component that
  ignores its config still opens `"default"`. Guest cooperation is not a security
  boundary; it is a convention with a test.
- **Rewrite tenant bytes to redirect `open()`.** Rejected in ADR-0012 and still
  rejected: it breaks digest pinning (ADR-0006) and it is the one thing a platform
  must never do to code it was handed.
- **Refuse keyvalue apps entirely until upstream lands.** Rejected: a dedicated host
  runs them correctly today, and refusing the catalog's stateful half to preserve a
  density target is optimising the wrong number.
- **Per-tenant NATS accounts under the shared host.** Not explored, and the honest
  reason is that nothing in this repo configures the host's NATS backend — it would
  be a new operational surface, whereas `environment` is one field that already
  works. Worth revisiting before the upstream ask, since it might be the same
  isolation at better density.
