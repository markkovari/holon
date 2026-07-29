# ADR-0019 — The density number, measured: 2.3 Mi per component, 70 Mi per app

- **Status:** accepted
- **Date:** 2026-07-29
- **Revises:** the *reason* to use wasmCloud here. Not a code change — a repositioning, with numbers.

## Context

A fair challenge, after ADR-0014 traded the multi-tenant density bet away: *is this whole
platform a waste, and should it just be the Kubernetes way — at the cost of density?*

The premise needed correcting first. **The multi-tenant density was already gone.**
ADR-0014 gave it up on evidence, so "go containers and lose density" was not the trade on
offer: both shapes are one pod per app. And on storage this design is *worse* than the
container way, because a NATS sidecar per app is **isolation by duplication**, where a
shared database with per-tenant credentials gets a stronger boundary at far higher
density.

Which left the real question: with both shapes at a pod per app, what does wasm still
buy? That number had never been measured, and it is the entire case. So it was measured.

## The measurement

Same eight components (`kv-probe`, 75 KB), same `poolSize: 8`, two shapes, idle, on one
node. Actual usage from `metrics-server`, not requests.

**Which runtime these belong to**, because two are in play and attributing them wrongly
would waste someone's day: the app hosts are `ghcr.io/wasmcloud/wash:2.5.2`, embedding
whatever wasmtime that release vendors — *not* the `wasmtime = "47"` pinned by this repo's
own `host/` crate, which runs the platform component, the e2e and the native vet-clinic
lane. Every figure below is wash 2.5.2's.

| | shape A: 8 components, ONE host pod | shape B: 1 component per host pod |
|---|---|---|
| pods | **1** | 8 |
| memory | **86 Mi** total | ~70 Mi *each* |
| warm wasm instances | 64 | 8 each (64 total) |
| per component | **10.8 Mi** | 78 Mi |

The comparison is instance-for-instance: `safe_pool_size` returned 8 in both shapes, so
shape A holds **64 warm instances in 86 Mi and one pod**, and shape B needs 560 Mi and
eight pods for the same 64.

Splitting that into a floor and a slope is what makes it useful:

```
host runtime floor (1 component):   70 Mi
same host with 8 components:        86 Mi
marginal cost per component:        2.3 Mi   (8 warm instances each)
```

Extrapolated, and the reason the original bet was attractive — **cold-start figures**, so
multiply by ~3 for a host that has served traffic ([ADR-0020](0020-the-density-number-under-load.md)):

| components | one host | pod per app | ratio |
|---|---|---|---|
| 10 | 91 Mi, 1 pod | 700 Mi, 10 pods | 8× |
| 100 | 296 Mi, 1 pod | 7 000 Mi, 100 pods | 24× less memory |
| 1000 | 2.4 Gi, 1 pod | 70 Gi, 1000 pods | 30× |

And the hop, measured from inside the cluster against a real workload:

```
in-cluster HTTP to a workload: p50 1.2 ms (min 0.9, max 9.2)
```

Every component boundary that becomes a *pod* boundary costs one of those. A four
component app linked in-process avoids three: **~3.6 ms per request and ~210 Mi.**

## Decision

**The platform's value is intra-app composition, not multi-tenant density. Say so, and
build for it.**

- **wasm wins decisively when one app is many components.** 2.3 Mi and no hop per
  component, against 70 Mi and 1.2 ms if that component were its own pod. This is
  "microservices without the network", and it is exactly what the 109-component catalog
  is for. `linked` and `fused` are the product; the strategy choice (ADR-0005) is the
  feature.
- **wasm ties or loses for single-component apps.** A 70 Mi host to run a 75 KB
  component is a bad trade against a container whose floor is its language runtime: worse
  than a Go or Rust binary (~5–15 Mi), comparable to Node or Python, better than a JVM.
  **A one-component app has no business on this platform** — it should be a container, and
  the honest version of this product says that out loud.
- **The 24–30× prize is real and blocked upstream by exactly one thing.** Many apps per
  host is where wasm beats containers by an order of magnitude, and the only blocker is
  per-workload keyvalue partitioning (ADR-0012, ADR-0015). That is now a *quantified*
  upstream ask: fixing `hostInterfaces[].name` would cut memory for 100 apps by roughly
  **24×** (~7 GB → ~300 Mi), not a tidiness improvement. ("24×" is a ratio, not a version
  number — a real reader tripped on that phrasing.)
- **Nothing in the control plane changes.** The applier holding the only credential
  (0003), reconcile and drift (0004), digest pinning (0006), delete-by-label with an
  orphan sweep (0016), the push queue (0017) — all substrate-agnostic. If this ever
  becomes container-per-app, the rewrite is `render.rs`, one module. That is worth knowing
  before choosing.

## Consequences

- **The pitch changes.** Not "cheaper multi-tenancy" — that was falsified — but "decompose
  as far as you like without paying a pod or a network hop per piece". The numbers above
  are the pitch.
- **`fused` versus `linked` gets sharper.** Both avoid the hop; `fused` also avoids the
  runtime link. The ~30-instance nested ceiling is what pushes a big graph to `linked`
  (`vet-domain`: 16 components, 104 core modules, does not fuse), so the ladder is fuse
  what you can, link the rest, and never split into pods without a reason.
- **A per-app 70 Mi floor should be visible to the tenant**, because it is most of the
  cost of a small app and it does not shrink. Metering that is honest; hiding it is not.
- ~~These are idle floors, not throughput.~~ **Now measured — see
  [ADR-0020](0020-the-density-number-under-load.md).** Under load the ratio holds and gets
  better (identical throughput and CPU, 3.2× less memory, 36% lower p99), but the absolute
  figures here are **cold-start**: a host pod that has served traffic settles near 233 Mi,
  not 86 Mi. Quote 0020's numbers publicly, not these.
- **One component was measured, small.** `kv-probe` is 75 KB. A 173 KB `mesh-domain` or a
  1 MB `wit-reflect` will have a larger slope. The 2.3 Mi is a floor for small components,
  not a universal constant.
- **A design wart surfaced while measuring**: push and re-apply share one interval, so a
  long `--reapply-interval` silently stops pushes (nothing was deployable for ten
  minutes). They want separate intervals, or a trigger on upload.

## Alternatives

- **Keep claiming density and hope upstream lands.** Rejected: the claim is false today
  for the shape we ship, and a plan resting on someone else's fix is not a plan.
- **Switch to container-per-app now.** Defensible, and cheaper for single-component apps.
  Rejected as the *default* because it forfeits the one thing measured here to be worth an
  order of magnitude — and because the control plane would survive the switch anyway, so
  the option stays open at low cost.
- **Both: containers for plain apps, wasm for component graphs.** The honest end state,
  and where this points. Not built, because it doubles the substrate before either half
  has a user.
- **Drop the NATS sidecar and share one bus with per-tenant credentials**, the way a
  database would. This is the direct answer to the "isolation by duplication" objection and
  would remove the per-app PVC. Blocked on the same upstream gap: `wash host` takes no NATS
  credential flags (only TLS), so there is no way to hand one host a scoped identity.
