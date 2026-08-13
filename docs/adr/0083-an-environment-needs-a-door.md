# ADR-0083 — An environment needs a door

**Status:** accepted
**Amends:** [ADR-0078](0078-an-environment-is-a-derived-app.md) — "No ingress."

## What ADR-0078 decided, and why it was right

An environment is a derived app: `shop` + env `branch-a` becomes
`shop-env-branch-a`, and because a component's bucket is named after its app
(ADR-0023), deriving the name gives the environment its own store with no new
isolation machinery. That part is unchanged and is the whole trick.

It also decided environments get **no ingress**, for a reason that is still
correct:

> An environment is somewhere to explore, not a front door. Giving it the
> parent's hostname would make two apps answer to one name — the ingress would
> route to whichever it saw last.

Two apps on one hostname is a real bug. Nulling the ingress does prevent it.

## What that made impossible

A branch of a swarm is not somewhere to explore. It is something to **drive**:
handed a plan, asked for a result, and asked again with what the checks found.
An app with no address cannot be driven from outside, and nothing inside a
freshly spawned environment starts on its own.

So every branch of a generation was a concurrent HTTP call to ONE app, each
carrying its own base tree in the request. That works exactly as long as a branch
keeps nothing between calls — and the first thing a branch wants to keep is the
first thing that makes branches worth having: a compiled artifact, a partial
index, the git objects it wrote. All of those live in a store, and the store is
the thing an environment exists to give it.

The result was a system with per-branch stores that no branch could reach.

## Decision

**An environment gets a derived hostname, not the parent's and not none.**

    parent                    swarm.ada.test
    env `branch-0`   branch-0.swarm.ada.test
    env of that      x.branch-0.swarm.ada.test

The environment's name, as a DNS label, prefixed onto the parent's host.

## Why this does not reintroduce what 0078 was avoiding

The hazard was *sharing* a name, not *having* one:

* It cannot collide with the parent — the parent's host is a strict suffix.
* Two environments collide only if their names do, and `spawn_environment`
  already refuses a duplicate name with a 409.
* Environment names are already restricted to what survives collapsing to a DNS
  label, for the same reason the store name is — two names that collapse together
  would share a bucket, which the name check exists to prevent.
* Nesting composes without a special case, because prefixing is associative.

An environment is now reachable by anyone who can reach the ingress, which is a
real change: it is exposure that did not exist before. It is the same tenant, the
same org and the same policy as the parent, and an address is the minimum a
branch needs to be work rather than scenery.

## How it is checked

`envbranch.rs`: three environments of one app, all writing **the same key**,
each reading back its own value, and the parent finding nothing.

The same key on purpose. Different keys per branch would pass just as happily
against a single shared bucket, which is the bug worth looking for.

### The first version of that test passed against the bug

It called each branch, kept the raw body, and asserted
`body.contains("branch-0")`. With the ingress nulled it still passed — because
the ingress, unable to route, answers with a message quoting the host it could
not route:

    no replica of "branch-0.swarm.ada.test" is currently placed

`contains("branch-0")` is perfectly true of that. **An error that quotes what you
asked for will satisfy any substring check for what you asked for.** The
assertions now parse the probe's JSON and compare the value; with the ingress
nulled the test fails on the first branch, saying an environment with no address
has no door.

## What is still not solved

Nothing gives a branch's environment a *different* manifest from its parent's.
An environment is a copy, so every branch runs the same components with the same
config, and a generation that wanted to try a different model per branch cannot
say so here — it varies the prompt instead (`generation::Strategy`).
