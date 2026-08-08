# ADR-0022 — Desired state is a manifest; the reconciler pulls it

- **Status:** accepted
- **Date:** 2026-08-08
- **Supersedes:** [ADR-0004](0004-reconcile-by-server-side-apply-on-save.md) (reconcile by server-side apply on save)

## Context

ADR-0004 made a save mean "render manifests and server-side apply them", with the applier
re-applying current revisions on an interval to correct drift. The apply half dies with
Kubernetes (ADR-0021). The *pull* half was always the good part and it survives unchanged.

## Decision

**A save validates the graph, builds a JSON manifest, and stores it as a revision. That is
all a save does.** The native `comp-reconciler` polls the current revisions, reads observed
inventory from the lattice, diffs the two, and issues commands.

The reconciler is native for the same reason the applier was (ADR-0003): a wasm component
has no background, and reconciling needs a held subscription and a timer. The dangerous
capability moved rather than disappeared — it is now "can start code on every node" instead
of "holds a kubeconfig" — so it stays in a small process with no business logic, no
database and no user concept, because `platform-domain` is what tenants send HTTP to.

`git mv applier reconciler` rather than a new crate: keeping the name would have kept the
meaning, and "applier" means "the thing that talks to Kubernetes".

## What survives verbatim, and why

- **`continue` on a failed poll.** A failed poll means we know nothing, so we change
  nothing. Under Kubernetes, treating that as "no apps exist" would have deleted every
  platform-owned Host; on the lattice it would stop every running instance on the fleet.
  Same line, same comment, different disaster.
- **The same rule for an unreadable inventory**, which is the new half: an empty read is
  not an empty fleet.
- **"Pending IS has no digest."** Derived state cannot desynchronise from a queue nobody
  is keeping in step.

## The diff is a pure function

`plan(desired, observed, hysteresis, cfg) -> commands` lives in `reconciler/src/plan.rs`,
has no I/O and no clock, and is tested to destruction. This is where `render.rs`'s 17-test
discipline moved: same property (a pure function from a spec to what a substrate needs),
same reason to test it hard. The crate is `[lib]` + `[[bin]]` so a dry run is by
construction the same logic the loop runs.

## Commands are absolute, not deltas

**Measured, on two machines.** The reconciler reconciles every 3 s and a host heartbeats
every 5 s, so the loop re-derived the same deficit against inventory that had not caught
up and issued the increment again. Two nodes ended up holding six replicas of a
two-replica app.

A command therefore says what the world should **be**, never what to change by. A repeated
`start` is a no-op — which is the idempotence the whole "re-derive from scratch every
pass" design already assumed it had, and had not actually got. `stop` means gone; shrinking
to a smaller non-zero count is a `start` with a lower count, so exactly one code path
changes a replica count and one removes an instance.

## Anti-thrash

Asymmetric hysteresis: scale **up** on the first observed deficit, because
under-replicated is the bad direction; scale **down** only after the same surplus persists
across passes. A missed heartbeat is not death — only KV expiry is. Commands are capped
per pass so a mass event drains instead of stampeding the survivors, and the number
dropped is reported rather than silently truncated.

All three constants are flags, not constants. They are guesses until there is real churn
to calibrate them against, and a guess baked into a binary is a guess nobody can fix at
3am.
