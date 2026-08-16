# ADR-0006 — Artifacts are digest-pinned OCI; the WIT surface is the contract

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

Tenants upload components, use built-in ones, or use someone else's public ones.
All three need one answer to "what exactly is this thing, and what does it
require", and the answer cannot come from the binary alone.

Two facts force the design:

1. **A built component is anonymous.** The embedded type says
   `package root:component; world root { ... }` — the source package and world name
   are gone, because `wac plug` doesn't need them. Since moving to
   `wasm32-wasip2`, there isn't even a `component-name` section unless someone
   stamps one; this repo's `just build` does (ADR: see `README.md` Toolchain), but
   a component uploaded from anywhere else arrives with no name at all. A binary
   cannot be trusted to say what it is.
2. **Tags drift and registries lie.** `examples/jobs/k8s/jobs.yaml` references
   `jobs-domain-golem:0.1.2` while the recipe pushes `:0.1.0` — a live, currently
   broken deploy caused by nothing but a mutable tag. The in-cluster registry has
   no auth, no TLS, and ephemeral container-filesystem storage, so a tag's meaning
   can change or vanish between a save and a re-apply.

## Decision

**Every artifact is addressed by digest, and identity lives outside the binary.**

- A component version in the catalog is a row: `{tenant, name, version, digest,
  surface, visibility, uploaded_at, uploader}`. The **digest is the identity**;
  the name and version are labels the platform assigns, never read from the wasm.
- **The `surface` is the contract**, and it comes from `wit:reflect`'s `inspect` at
  upload time: exports, composable imports, host imports, nested instance count,
  size, sha256. Reflection is also validation — a truncated upload or a core
  module is refused here rather than becoming a broken catalog row.
- **Rendered manifests always pin `image: <registry>/<repo>@sha256:<digest>`**,
  never a tag. This is what makes ADR-0004's periodic re-apply idempotent: a
  re-apply of revision N deploys exactly the bytes revision N deployed.
- Tags may exist for human convenience on push, but nothing in a manifest or a
  deployment record ever references one.
- **The same digest is never re-inspected**: surfaces are cached by digest, which
  also means two tenants uploading identical bytes share one catalog entry's
  surface (not its visibility row — see ADR-0007).

## Consequences

- The registry must be **content-addressable and durable before tenants exist**.
  Today's `registry:2` with no PVC, no auth and no TLS is a phase-0 crutch: a
  restart loses every image, which under digest pinning turns every deployment
  un-re-appliable rather than merely stale. Durable storage plus authentication is
  a slice-1 prerequisite, not a hardening task.
- Push happens from the platform (server-side), not from a laptop, which retires
  the `localhost:30500` vs `registry.<ns>.svc.cluster.local:5000` asymmetry the
  Justfile currently lives with. The platform pushes and pulls by the in-cluster
  name.
- Because identity is external, **the platform can and must display provenance**:
  who uploaded this digest, when, and whether it was built here. "Trust me, it's
  called auth-guard" is not something a binary gets to claim.
- `wit:reflect` becomes load-bearing infrastructure, not a demo. Its `inspect` is
  the gate every artifact passes; its planner decides what a graph can do
  (ADR-0005). It needs to stay small and boring.
- Signing is not in this ADR but is enabled by it: a digest is exactly what a
  cosign signature covers. ADR-0007 makes signing a requirement at the point it
  matters (public sharing), and the digest-first decision here is what lets that
  be added without a migration.

## Alternatives

- **Tag-based references** (`name:version`). Rejected: the repo has a live broken
  deploy from exactly this, and mutable tags make "re-apply the same thing"
  impossible to guarantee.
- **Store the wasm in `blob:store`, no registry.** Rejected: the operator pulls
  images from an OCI registry — that's the interface the runtime actually has. A
  blob store would need the platform to serve an OCI API in front of it, which is
  a registry with extra steps. (`blob:store` remains right for the *upload*
  staging area before push.)
- **Trust the `component-name` section for identity.** Rejected on the evidence:
  p2 artifacts have none unless stamped, stamping is ours and not universal, and a
  name in a file uploaded by a tenant is a claim, not a fact.
- **Re-derive the surface from source WIT.** Rejected: tenants upload binaries, not
  source trees. This is also why `tools/gen-catalog.py`'s regex-over-source
  approach cannot be the platform's catalog — see docs/apps/STUDIO.md.
