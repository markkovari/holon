# 0078 — An environment is a derived app

Status: accepted, and built (the desired-state half). The first piece of a
graph-engineering loop: many parallel environments of one graph, explored
concurrently and independently.

## What is being asked for

Not blue/green. The goal is an agent-driven loop where a graph forks — each node
explored in its own environment, in parallel, without the branches touching each
other's state — and where the *running system* decides what should exist rather
than a person editing a manifest.

That last clause is the difference from wadm, which was the obvious comparison to
make. wadm's convergence is dynamic and its *intent* is static: desired state is
a document a human applies. Here desired state has to be authored by the
workload, because what should be running is a function of what the graph is
currently exploring.

## Desired state is not optional

The tempting shortcut is to have a component ask a host to start something. It
does not survive one reconcile pass. `plan()` stops any instance no manifest asks
for — *"nothing wanted here at all: take it off this node"* — so anything started
behind the loop's back is reaped within an interval.

So spawning **writes desired state** and the reconciler converges on it, exactly
as it does for a human's deployment. The control loop stays the single authority
on what runs; what changes is who is allowed to write its input.

## The trick: an environment IS an app

```
shop                ingress=yes   store=b-app-ada-shop
shop-env-branch-a   ingress=none  store=b-app-ada-shop-env-branch-a
shop-env-branch-b   ingress=none  store=b-app-ada-shop-env-branch-b
```

A store is `b-app-{tenant}-{app}` (ADR-0023), so deriving the app name gives each
environment its own store **with no new isolation machinery**. Placement,
scaling, links, cross-node invocation and reaping all keep working because
nothing below the platform knows this app was born differently.

Two deliberate choices:

- **No ingress.** An environment is somewhere to explore, not a front door.
  Copying the parent's hostname would make two apps answer to one name and the
  ingress would route to whichever it saw last.
- **A name collision is refused, not resolved.** `shop` + env `x` derives
  `shop-env-x`, and an app genuinely called `shop-env-x` collapses to the same
  DNS label and therefore the same store. Same tenant, so not a cross-tenant
  leak, but still two apps in one store. Refused.

## Cost, measured before choosing

An environment sharing the node's host costs **~2.3 MiB** and starts in **0.08
ms** when that digest is already compiled there (ADR-0019, ADR-0052 — one copy of
machine code per digest). Thirty-two idle apps on one digest measured 48 MiB
total (ADR-0053). Fifty forks of one graph is a plausible thing to do.

## What this deliberately does not do: clone the host

The other option is a host process per environment, and it is written down here
because it will come back.

**For it:** true crash and OOM isolation; OS-level resource limits; a separate
state dir, which is what a per-environment git/virtfs actually wants; a
*different host version* per environment, which is how you would ever canary a
runtime change; and a plausible route to snapshotting an environment.

**Against it:** compiled-code sharing is lost, so ~35 ms and 12–50 MiB per
environment instead of 0.08 ms and 2.3 MiB (ADR-0034, ADR-0040); a supervisor,
child lifecycle, per-child node identity and orphan cleanup, none of which exist;
and every environment becomes a lattice node, which multiplies
[ADR-0056](0056-a-converged-app-keeps-its-placement.md)'s `apps × nodes` pass
from both sides at once. [ADR-0072](0072-one-loop-at-a-time.md) declined sharding
*because post-change passes are rare* — agent-driven spawning is precisely the
thing that would falsify that.

The likely answer is neither-always: `isolation: shared | process` per
environment, defaulting to shared, with process reserved for code that is
untrusted, crash-prone, or needs its own filesystem. A middle setting worth
naming is one host per **graph run** rather than per node — crash isolation
between concurrent runs, compiled-code sharing within one.

## Where capabilities come from, since it decides how often the host changes

A capability belongs in the host only if it needs something only the host has:
the OS, the network, the store backend, the process. That is a short list —
`wasi:keyvalue`, `wasi:http`, `wasi:config`, `comp:secrets/reader`,
`comp:store/cas`. Everything else is a component: `record-store`, `blob-store`,
`cache`, `fsm-workflow`, `event-bus`, `ai-inference` and a hundred more, linked
at runtime and deployable by an agent mid-run.

So a graph engine grows by adding **components**, not host versions — including,
probably, the git-backed filesystem, which is content-addressed objects plus refs
and therefore expressible over `blob-store` and `record-store` without OS access.
The host stays small and boring, which is what you want from the thing that
enforces every boundary: a host rebuilt often is a host whose isolation you
re-verify often.

## Three bugs on the way in, all the same bug

The spawn lookup 404'd on a deployment that plainly existed, three times:

1. `find_one(DEPLOYMENTS, "id", …)` — deployments are indexed on `org` and
   `tenant` only, so that was never a lookup at all;
2. `find_one(DEPLOYMENTS, "name", …)` — same reason;
3. and once found, `newest_revision(name)` came up empty because revisions are
   keyed by the deployment's **record id** while a manifest's `app` — and hence
   the store name — is its **name**.

One underlying mistake: assuming which key a collection is addressable by instead
of reading it. `find_by` silently returns nothing for an unindexed field, which
is indistinguishable from "no such row" — the same silent-empty shape ADR-0075
was written about.

## Still to build

- **The guest capability.** A component calling `spawn` is the actual ask; today
  it is an HTTP route a person or an agent-outside-the-graph calls.
- **Quota.** A component that can create instances can exhaust a fleet.
  `quota:meter` already exists and nothing is metering this yet.
- **Lifecycle.** Nothing expires an environment nobody despawned.
