# ADR-0015 — A bucket name is not a boundary, and `hostInterfaces[].name` does not work

- **Status:** accepted
- **Date:** 2026-07-28
- **Confirms:** [ADR-0014](0014-an-application-owns-a-host.md) — measured on the cluster, including the loopback sidecar
- **Closes:** the "one host per tenant, apps separated by interface names" alternative

## Context

ADR-0014 shipped a host per application, and priced it honestly: one pod per app,
worse cold start, scale-out capped at one host. The obvious cheaper design was raised
immediately — **one host per tenant, with apps distinguished by labels and named
capability instances** — and the CRD appeared to support it:

> `hostInterfaces[].name` — "Name uniquely identifies this interface instance when
> multiple entries share the same namespace+package. Components use this name as the
> identifier parameter in resource-opening functions (e.g. `store::open(name)`).
> Required when multiple entries of the same namespace:package exist."

That reads like per-app buckets on a shared host. It also implied ADR-0012's
conclusion was too pessimistic: the entry list is platform-authored per workload, so
if `name` selected the store, a component could only reach names the platform gave
it — enforcement by manifest, not cooperation.

`hostInterfaces[].name` had **never been used in this repo**, in any manifest. So it
was tested, with a purpose-built instrument (`components/kv-probe`, whose bucket comes
from the query string because every catalog component hardcodes `open("default")`).
The setup was deliberately hostile to the hypothesis: **one** host, **one** data NATS,
three workloads differing only in the entry name.

## Decision

**The platform never emits `hostInterfaces[].name`, and never treats a bucket name as
an isolation boundary.** Storage separation comes from the data plane the host is
pointed at (ADR-0014), and from nothing else.

Two measurements force this.

### 1. Setting `name` breaks the workload entirely

Same image, same `interfaces: [store]`, same everything — one workload with
`name: alpha`, one without:

```
alpha (name: alpha)   READY Unknown   <- never links
beta  (no name)       READY True
```

```
component imports instance `wasi:keyvalue/store@0.2.0-draft`, but a matching
implementation was not found in the linker
Caused by:
    0: instance export `bucket` has the wrong type
    1: resource implementation is missing
```

Reproduced across four probe builds while other hypotheses were eliminated (the
resource's method set, importing `batch`, importing `wasi:config`, declaring
`atomics`/`batch` entries). The field is documented, required by its own docs when two
entries share a namespace+package, and **unusable** on wash 2.5.2 — which is worth an
upstream issue with the repro above.

### 2. Even if it worked, the name is chosen by the guest and unrestricted

On one host, two workloads, no `name` anywhere:

| probe | result |
|---|---|
| `open("shared")`, `open("nobody")`, `open("anything")` | all **succeed** — no manifest restricts the identifier |
| workload A writes `shared/x`, workload **B** reads `shared/x` | `found: true` — same name, same store |
| A reads a name nobody wrote | `found: false` — different name, different store |
| A writes into `alpha-private`, a name belonging to another app | **succeeds**, and B reads it back |

So the bucket namespace on a host is **flat and global**, and any component can open
any name in it. A per-app naming scheme would be a naming *convention* that a buggy
or hostile component ignores by opening its neighbour's name. That is not a boundary,
and ADR-0012's conclusion was right for a better reason than it gave.

### 3. ADR-0014's design measured, and one bug in it

The same run validated what ADR-0014 said was unproven:

- A `wash host` pod rendered as a **plain Deployment registers itself** as a `Host`
  and goes ready.
- A workload pinned by `template.spec.environment` **schedules onto exactly that
  host**, across namespaces.
- `wasi:keyvalue` **binds and works** against a **loopback NATS sidecar**
  (`--data-nats-url=nats://127.0.0.1:4222`).
- The decisive one: with app A on the shared bus and app B on its own loopback bus,
  `shared/x` reads `found: true` on A and `found: false` on B; B writes its own
  `shared/x` and the two values coexist. **The cross-app read that succeeded on one
  host returns not-found across hosts.**

And one real bug in ADR-0014's rendered manifest, now fixed: the sidecar's
`startupProbe` was `tcpSocket`, and **kubelet dials probes at the pod IP**, so a
loopback-only bind is refused forever — 25 probe failures, 5 restarts, pod stuck in
`PodInitializing`. It must be an `exec` probe (`nc -z 127.0.0.1 4222`), run from
inside the container where `127.0.0.1` means what we meant.

## Consequences

- **ADR-0014 stands, on evidence rather than on documentation.** The pod-per-app cost
  is real and now known to buy something real.
- **`hostSelector` and host labels exist** (a host advertises
  `labels={"hostgroup": "default"}`) and are untested here. They are placement tools,
  not isolation tools, and `environment` already does the placement ADR-0014 needs —
  so they stay unemitted for now, for the same reason as before: nothing has proven
  them.
- **The upstream ask sharpens.** ADR-0013 asked for "a keyvalue allow-list mirroring
  blobstore's `buckets:`". The precise ask is now: *make `hostInterfaces[].name` work,
  and make the identifier a component passes to `open()` resolve only within the names
  its own workload declares.* That single change would restore per-app isolation on a
  shared host and undo ADR-0014's density cost.
- **`components/kv-probe` stays in the tree.** It is the reproduction case for the
  upstream bug and the regression test for the day the operator is upgraded — the
  question it answers is not answerable by any other component here, because they all
  hardcode their bucket.
- **A deleted host leaves its `Host` object behind.** Both experiment hosts remained
  registered in the operator's namespace after their pods and namespace were deleted,
  and had to be removed by hand — nothing else reaps them. Chasing this turned up that
  the platform had **no delete path at all**, so every app ever created leaked its
  whole footprint. Resolved by
  [ADR-0016](0016-deleting-an-app-is-reconciled-not-remembered.md): deletion is a
  label-scoped prune plus an orphan sweep in the reconcile loop, with the reserved
  `app-` environment prefix as the ownership marker that keeps the sweep away from
  hosts the platform does not own.

## Alternatives

- **One host per tenant, apps separated by `hostInterfaces[].name`.** The proposal
  this ADR tested. Rejected on measurement, twice: the field breaks binding, and the
  name space it would partition is guest-addressable anyway.
- **One host per tenant, apps separated by a bucket-name convention** (each app told
  its prefix via `wasi:config`). Rejected: measurement 2 shows a component can open
  any name, so the boundary holds only while every component behaves. That is a
  correctness convention for one user's own apps at best, never a boundary between
  tenants.
- **One host per tenant with per-tenant NATS accounts.** Still the interesting dense
  option, still blocked on `wash host` taking no NATS credential flags (only TLS).
  Unchanged from ADR-0014.
- **Wait for the upstream fix before shipping.** Rejected: ADR-0014 works today, and
  the upstream change would be a renderer edit, not a redesign.
