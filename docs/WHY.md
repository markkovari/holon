# Why this platform

> **Correction (2026-08).** The falsification below was of the *wasmCloud-hosted*
> design, and it has been reversed. The leak was a capability provider choosing a
> bucket from a guest-supplied string; owning the host means the linker names the
> bucket instead, and a guest string became a key into host-side state
> ([ADR-0023](adr/0023-isolation-is-a-linker-boundary.md)). Multi-tenant density is
> back and measured: two organisations on one fleet with cross-org reads refused
> ([ADR-0033](adr/0033-two-orgs-under-load.md)), every node holding more than one org
> ([ADR-0034](adr/0034-two-machines-one-fleet.md)), nodes idling at 12 MiB. See
> [`CURRENT.md`](CURRENT.md) for what the platform actually is today.

> Every number here is measured on a real cluster and traceable to an ADR. Where a
> claim is unproven it says so. The original pitch — cheaper multi-tenancy through
> shared hosts — was falsified *on wasmCloud* during the build
> ([ADR-0012](adr/0012-keyvalue-isolation-needs-a-cooperative-component.md),
> [ADR-0014](adr/0014-an-application-owns-a-host.md)) and later recovered by owning
> the host; this document is the argument that survived either way.

## The claim, in one sentence

**Split your application into as many pieces as the design wants, and pay almost
nothing for the split.**

## The problem

Today the question "should this be its own service?" is not answered by design. It is
answered by operational cost. Every split you make buys you a deployment, a Service, a
scaling policy, a dashboard, a new failure mode — and a network hop on the request path.
So teams do one of two things: they under-decompose and live with a monolith they cannot
reason about, or they decompose properly and pay the microservices tax forever.

The tax is measurable. On this cluster, one component in its own pod costs **70 Mi** and
each boundary crossing costs **1.2 ms p50** ([ADR-0019](adr/0019-the-density-number.md)).
A ten-piece app is therefore 700 Mi and up to nine hops before it does any work.

## What this platform charges instead

Same measurement, same components, same warm-instance count — the only difference is
whether the pieces share a host:

| | 8 components, one host | 8 components, one pod each |
|---|---|---|
| pods | **1** | 8 |
| memory | **86 Mi** | 560 Mi |
| network hops between them | **0** | up to 7 |

The shape of it: a **70 Mi floor** for the host runtime, then **2.3 Mi per additional
component** — each with eight warm instances ready to serve. (Those are cold-start figures;
a pod that has served traffic settles nearer 233 Mi, and the *ratio* is what holds.)

**Under load it gets better, not worse.** Same 200 connections on one node, arranged two
ways ([ADR-0020](adr/0020-the-density-number-under-load.md)):

| | one pod, 4 components | four pods, 1 each |
|---|---|---|
| throughput | 20 041 rps | 20 064 rps |
| CPU | 5 078 m | 5 172 m |
| memory | **257 Mi** | 820 Mi |
| **p99** | **16.5 ms** | 26.0 ms |

Identical throughput, identical CPU, **3.2× less memory, and a 36 % better tail** — fewer
pods means fewer independent schedulers and pools on one request's critical path. Adding
three components to a host cost 1.6 % throughput; a million requests over 60 s showed no
leak. So this is not a memory-for-speed trade: packing is both cheaper and steadier.

So the decomposition decision stops being an economic one. Four components or forty, it
is one pod, one endpoint, one thing to operate, and no hop between the pieces.

## Why that is possible at all

Five properties, each doing real work. None of them is available to a container platform.

**1. Components have typed boundaries.** A wasm component declares its imports and
exports as WIT interfaces, so a composition can be *checked* before anything deploys. The
platform runs `wac`'s subtype checker over the graph and refuses an edge that does not
fit, naming the node and the interface. HTTP between containers has no types at the
boundary, so the equivalent error arrives in production as a 500.

**2. Two ways to wire, neither of them a network.** `fused` composes the graph into one
artifact at build time — direct calls, nothing to configure. `linked` hands N components
to one runtime which wires them in-process, which allows a shared capability instance and
even cycles. The tenant picks; the planner refuses a strategy the graph cannot support
(a cycle under `fused`, an instance count over the ceiling, an unsatisfied import). Both
are proven live: four components, one pod, a request traversing all four
([ADR-0018](adr/0018-the-platform-deploys-a-running-app.md)).

**3. Capabilities are declared, not ambient.** A component's imports *are* its permission
list. It cannot open a socket, read a file or reach a bucket that was not granted, because
the capability simply is not in its world. This is enforced by the module system, not by a
policy engine: omit an interface and the host refuses to instantiate the component at all
— measured, with the linker error to prove it. Egress is a per-component fail-closed
allow-list, which is finer-grained than a container platform can express (a NetworkPolicy
binds a pod, not a library inside it). On this cluster it was in fact *the only* control
that held, because nothing enforces NetworkPolicy here at all.

**4. The artifact is the contract.** Identity lives outside the binary: every deployment
pins `repo@sha256:…`, never a tag, so re-applying revision N deploys exactly the bytes
revision N deployed. The component's surface is extracted by reflection at upload, which
doubles as validation — a truncated file or a core module is refused at the door rather
than becoming a broken catalog row. The push is a pure function of the bytes: same input,
same digest, verified.

**5. Kubernetes does the hard parts.** The platform renders manifests and hands them to a
small native process that holds the only cluster credential; scheduling, restarts, storage
and quota stay with Kubernetes. That process refuses anything outside an allow-list of
kinds, any object aimed at another namespace, any image but its own two, and any pod
asking for privilege or a service account. Deletion and drift are the same mechanism —
reconcile from stored revisions — so a half-finished delete, a crash or a hand-edit all
converge. Proven by deleting a Service and scaling an app's host to zero: one pass later
both were restored and the app answered with its data intact.

## Running it yourself

For your own apps on your own machines, most of the platform is unnecessary — the
multi-tenancy machinery defends strangers from each other. [`SELFHOST.md`](SELFHOST.md)
is the progressive path: `comp-host` + systemd + a per-app URL to start (built, one
`just selfhost-deploy` away), many-apps-per-host when RAM demands it, k3s + the operator
only when placement across machines becomes a chore.

## What it is honestly not for

- **A single-component app.** You would pay a 70 Mi host to run a 75 KB component. Use a
  container: a Go or Rust binary's floor is 5–15 Mi. This platform earns its keep from
  the *second* component onward.
- **Many tenants packed onto one host.** That was the original bet and it is currently
  impossible: `wasi:keyvalue` cannot be partitioned per workload, so each app gets its own
  host and its own bus. The prize is large — **~24× less memory** for 100 apps (~300 Mi
  instead of ~7 GB) — and blocked on one upstream fix, which is now a quantified ask
  rather than a wish.
- **High-density shared state.** A private message bus per app is isolation by
  duplication. A shared database with per-tenant credentials is denser *and* a stronger
  boundary. Until a host can be handed a scoped NATS identity, this is a real cost.
- **Ecosystem maturity today.** HPA, tracing, service meshes, ingress controllers and
  every engineer's existing mental model all belong to containers.

## What is proven, and what is not

**Proven on a cluster** ([ADR-0018](adr/0018-the-platform-deploys-a-running-app.md),
[ADR-0019](adr/0019-the-density-number.md)): sign in, upload, reflect, push, deploy, serve
— both strategies; per-app storage isolation (two apps of one tenant, same bucket and key,
two values); two tenants isolated at the API and in storage; delete removing an app's whole
footprint including the host's self-registered object; drift corrected automatically; and
the density and hop numbers above.

**Not proven**: more than one node; a large component (everything here used a 75 KB one, to
isolate platform overhead rather than app cost); behaviour at `maxInvocations`; a hostile or
hot neighbour inside a packed host; and anything longer than a minute of load. Also unbuilt: tenant configuration (a deployed app
cannot yet be told anything), rollback (revisions are stored, the verb is missing), secrets,
public sharing, and the platform hosting itself.

That list is short, specific, and none of it is load-bearing for the claim at the top —
which is the only reason this document is worth writing yet.
