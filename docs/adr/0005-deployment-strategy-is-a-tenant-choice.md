# ADR-0005 — Deployment strategy is a tenant choice: fused or linked

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

A tenant's app is a graph of components. There is more than one legitimate way to
turn that graph into something running, the repo has already built all of them by
hand, and they are **not equivalent** — they differ in instance count, in what can
be debugged, in what a cycle means, and in whether a capability is shared.

What exists today, per STUDIO.md and the studio's three emitters:

| | fused (`wac`) | linked (one workload) | separate workloads + v1 `link` traits |
|---|---|---|---|
| when wiring happens | build time | run time, in-process | run time, over wrpc/NATS |
| shared capability instance | no — a copy per socket | yes, one component | yes |
| cycles | impossible | legal | legal |
| nested-instance ceiling | **~30, hard** | not applicable | not applicable |
| per-call cost | none (direct call) | in-process | a network hop |
| lane in this repo | 55 `wac plug` recipes | `vet-domain-v2.yaml` (16 components) | `infra/k8s/app.yaml`, `k8s-collapse` |

The ceiling is not theoretical: `vet-domain` fused whole is 104 core modules and
**does not deploy** — that is why `vet-domain-lattice` exists, fusing the six
pure-compute capabilities and keeping the stateful ones linked. `wit:reflect`
already computes the instance count and warns before the build.

One correction to the brief that shaped this ADR: **"host plugins" are not an
extension point we can offer.** In the v2 model, "plugin" means the host's
*built-in* capability implementations — keyvalue backed by NATS, `wasi:http` by
the host's `:9191` virtual-host router, blobstore by the NATS plugin. A grep of
`infra/` and `examples/*/k8s/` for "plugin" returns zero hits: nothing in this
repo authors, registers or configures one. Per-tenant KV backends via a custom
plugin is not a path that exists today; the tracked route is upstream
multi-backend `hostInterfaces` (#5051).

## Decision

Expose **two strategies** as an explicit per-deployment choice, and make the
platform responsible for telling the tenant which one their graph can actually
use:

- **`fused`** — the platform composes the graph into ONE artifact with
  `wit:reflect`'s `compose` (the same `wac_graph::plug` the CLI runs), pushes that
  artifact, and deploys a single-component `WorkloadDeployment`.
- **`linked`** — the platform pushes each component separately and deploys them as
  N components in ONE `WorkloadDeployment`, letting the runtime link them
  in-process. Composable edges appear nowhere in the manifest; only host
  capabilities do, as `hostInterfaces`, **one entry per interface**.

Capabilities the *host* provides (`wasi:keyvalue`, `wasi:http`, `wasi:config`,
`wasi:blobstore`) are never a strategy choice. They are always `hostInterfaces`,
always stamped by the platform (ADR-0008), in both strategies.

The choice is **validated, not merely accepted**. On save the platform runs
`wit:reflect`'s planner and refuses a strategy the graph cannot support:

- `fused` + a cycle → refused (a static composition is a DAG);
- `fused` + estimated instances over the ceiling → refused, with the count and a
  pointer at `linked` or at the lattice pattern (fuse the pure-compute ones);
- `linked` + a graph whose edges the runtime cannot wire → refused;
- either + unsatisfied non-host imports → refused, naming node and interface.

Default for a new deployment: **`fused`**. It has 55 working precedents in this
repo, its failure mode is a build error the tenant sees immediately rather than a
binding that silently didn't happen, and it needs no `hostInterfaces` reasoning
per component.

The v1 OAM `link`-trait lane (separate workloads wired over wrpc) is **not
offered**. It is the lane whose rollout hazard needed the `k8s-collapse`
workaround, it costs a network hop per call, and the repo's v2 path supersedes it.

## Consequences

- Two render paths and two debug stories, from day one. This is the accepted cost
  of the differentiating feature; it doubles the renderer's golden-file tests.
- `fused` makes the platform a build service: composition happens server-side, so
  compose time and artifact size land on the platform's budget, and a failed
  compose must surface as a readable error rather than a 500.
- `linked` puts the platform on the hook for `hostInterfaces` correctness, where
  the failure mode is nasty: an entry binds to a component **only if that
  component's world covers every interface listed**, so a merged `[store, atomics]`
  entry silently skips components importing only `store`. The renderer must emit
  one entry per interface, and a test must assert that (it is the exact bug the
  vet-clinic manifest carries a comment about).
- The tenant sees a real engineering tradeoff in the UI, with the numbers
  (instance count, artifact size, shared-vs-copied capabilities) that
  `wit:reflect` already returns. That is a feature, not a leak.
- When #5051 lands, per-tenant KV backends become a `hostInterfaces` change in one
  renderer, not a new strategy. Worth re-reading this ADR at that point.

## Alternatives

- **Fused only.** Simplest, and enough for most graphs. Rejected because it cannot
  express a shared capability instance or a cycle, and it hits the ~30-instance
  ceiling on exactly the interesting apps (`vet-domain` proves it).
- **Linked only.** The v2-native answer with no build step. Rejected as the sole
  option because `hostInterfaces` mistakes fail silently, and because a tenant
  wanting one self-contained artifact (to run on the native host, or to hand to
  someone) has no way to get one.
- **Offer host plugins as a third strategy.** Rejected as not implementable today:
  plugins are the host's built-ins, not an authoring surface, and nothing in this
  repo has ever registered one.
- **Infer the strategy, never ask.** Tempting, and the planner has enough
  information to pick. Rejected because the choice has consequences a tenant can
  care about (shared instance state, one artifact vs many), and silently switching
  strategies between saves would change runtime semantics without telling anyone.
  The platform *recommends*; the tenant decides.
