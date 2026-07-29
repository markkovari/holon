# ADR-0017 — The applier pushes, and the registry is a cache

- **Status:** accepted
- **Date:** 2026-07-28
- **Revises:** [ADR-0006](0006-artifacts-are-digest-pinned-oci.md) — durability and authentication, not digest pinning

## Context

Uploading a component worked and deploying it did not. `POST /api/components`
reflected the wasm, stored the surface and staged the bytes in `blob:store`, but
`oci_ref` stayed empty and a save therefore returned `409 not in the registry yet`.
`POST /api/internal/pushed` existed as a seam with nothing calling it. **A tenant
could not deploy a component they had uploaded** — the single gap between the platform
working and being usable.

Two facts about the environment decided the design, and neither is about storage.

**`wasi:http` validates TLS against webpki roots.** That is the constraint that made
the applier exist in the first place (ADR-0003). A wasm component therefore cannot
speak to a registry presenting a private cert, and pushing to an authenticated external
registry from wasm would mean putting registry credentials in `wasi:config` — which
ADR-0010 forbids for the platform's own secrets, because `wasi:config` is a per-component
view the guest reads in full. **Whoever pushes decides which registries are reachable
forever.**

**The bytes are already durable.** They land in `blob:store` at upload, before any
push. So the registry does not have to be the system of record; it has to be the thing
the operator can pull from.

The registry that existed was `registry:2` with `volumes: null` — every restart lost
every image, which under digest pinning does not make deployments stale, it makes them
un-re-appliable. ADR-0006 predicted exactly this and called it a phase-0 crutch.

## Decision

**The applier pushes. The registry is in-cluster, PVC-backed, and a cache.**

- **The applier owns the push**, not the wasm component. Native code makes TLS and
  token flows unremarkable, credentials live as Kubernetes Secrets where the rest of
  the applier's do, and the registry's location becomes `--registry` rather than an
  architectural commitment. Choosing the other side would have locked the platform to
  plain-HTTP-in-cluster permanently.
- **It is reconciled, like everything else here.** "Needs pushing" is *derived* — it
  means *has no `oci_ref`* — so there is no queue with its own lifecycle to get out of
  step. The applier's existing loop asks `GET /api/internal/pending-pushes`, fetches
  bytes from `GET /api/internal/artifact?key=`, pushes, then calls the seam that already
  existed. A crash anywhere costs a repeated push, never a wrong one.
- **The bytes move by pull, not push.** The applier fetches them. The wasm side has one
  awkward outgoing-body handshake (learned the hard way — a save once hung for 728
  seconds) and no reason to stream megabytes through it, and a pull is the direction the
  applier already polls in.
- **Push happens in the same pass as apply, before it.** A manifest references an
  artifact by digest, so an unpushed component cannot deploy at all; pushing first takes
  an upload all the way to running in one interval instead of two.
- **The push is a pure function of the bytes.** `created` in the OCI config is pinned to
  the epoch rather than the wall clock, because it is part of the config blob and
  therefore of the manifest digest — a timestamp there would mint a new identity for one
  artifact on every retry. Same bytes, same digest, and a re-push is something the
  registry deduplicates. Verified: re-uploading a component produced the identical
  digest.
- **Four HTTP calls, no registry crate.** Start an upload, PUT the layer, PUT the
  config, PUT the manifest — on the `reqwest` client the applier already has. The reason
  to write it out is that **the media types have to match `wkg` exactly**, and they were
  read off a real artifact in the running registry rather than guessed:
  `application/vnd.oci.image.manifest.v1+json`, config
  `application/vnd.wasm.config.v0+json` (carrying the component's exports and imports,
  which upload-time reflection already knows), layer `application/wasm`. An abstraction
  that chose those for us is the last thing wanted here.
- **What is recorded is the MANIFEST digest.** A pull by digest resolves the manifest,
  not the layer; pinning the wasm's own hash would produce a reference that never
  resolves.
- **Repositories are `<tenant>/<id>`.** Two tenants may both own a component called
  `mesh-domain`. A shared repo path is harmless for correctness — blobs are
  content-addressed and references are digests — but it leaks one tenant's component
  names to anyone who can list the other's repository. (Found by a test, not by
  thinking.)
- **The registry is in-cluster**, in the platform's namespace, PVC-backed, no NodePort.
  Pulls stay local so no application restart depends on egress; it matches the tenant
  NetworkPolicy already written (egress to the platform namespace on `:5000`); and there
  is no external account or rate limit in the deploy path.
- **Durability lives in `blob:store`, not the registry.** The registry is a cache of it,
  which digest pinning makes trivially correct because artifacts are immutable. A lost
  registry costs re-pushes, not artifacts — which is what makes one `local-path` PVC an
  acceptable answer here and would not be otherwise.

**On authentication, this revises ADR-0006**, which called it a slice-1 prerequisite.
The wasmCloud host authenticates pulls from a docker config on disk (it logs `failed to
retrieve docker credentials` today), so requiring credentials means mounting a pull
Secret into **every app's host pod in every tenant namespace** — a new allow-listed
kind, per-namespace Secrets, and a new way for a deploy to fail. The threat it buys is
already closed: component egress is `allowedHosts` fail-closed and the platform never
allow-lists the registry, so **tenant code cannot reach it at all**. Only the host pod
running that code can, and that pod is ours. So the boundary is a `NetworkPolicy` on the
registry rather than a password, and the reasoning is written down where the object is.

**With one correction, measured after the fact: `NetworkPolicy` is not enforced on this
cluster at all.** A pod in an unlabelled namespace reached the registry (HTTP 200), and
`kube-system` contains only coredns, local-path-provisioner and metrics-server — no CNI
daemonset, no policy controller. OrbStack accepts policy objects and enforces none of
them. So on *this* cluster the registry's `NetworkPolicy` is documentation, and the only
thing actually keeping tenant code away from the registry is `allowedHosts`, which the
**wasmCloud host** enforces in the runtime rather than the network. That is the load
bearing control, and it is fail-closed (measured separately, on eshop). The policy stays
because it is correct on a cluster that enforces policies — but it must not be counted
as a second layer here, and anyone reading the auth argument above should read it as
resting on one control, not two.

## Consequences

- **Upload-to-deployable is eventually consistent**, bounded by the reconcile interval.
  Measured end to end against the real registry — and then all the way to a running app:
  upload → pending → pushed → `deployable: true` → deployed → **serving HTTP with its
  own keyvalue working** (ADR-0018). If the wait ever
  matters, the fix is to trigger a pass on upload — not to move the push into the wasm
  side, which would forfeit every registry that needs TLS.
- **The integrity check earned itself on its first run.** It fired immediately, and what
  it caught was that the catalog's `sha256` is `wit:reflect`'s **12-character display
  hash** (`hex12`, the convention `tools/gen-catalog.py` uses), not a full digest. It is
  therefore a prefix comparison and a corruption check — 48 bits, plenty for a mangled
  transfer — and *not* an authenticity check. The artifact's identity is the manifest
  digest the push returns. Worth revisiting if `wit:reflect` ever returns a full hash.
- **Reclaiming space is not solved.** `REGISTRY_STORAGE_DELETE_ENABLED` is on so a
  manifest can be deleted, but freeing blobs needs a `registry garbage-collect` run,
  which nothing here schedules. Deleting a component today leaves its blobs. An honest
  wart, not a hidden one.
- **`--validate-only` no longer means "touches nothing".** It means *builds no
  Kubernetes client*. A registry push is not a cluster write and is additive and
  content-addressed, so keeping it enabled is what let the push path be proven with no
  cluster at all. `--no-push` is the off switch.
- The external options stay open precisely because the applier pushes: `--registry`
  plus credentials points at ghcr/ECR, and an in-cluster pull-through cache in front of
  an external source of truth is a configuration, not a redesign. Under digest pinning a
  cache needs no invalidation.

## Alternatives

- **The wasm component pushes.** Rejected on the TLS constraint above: it would cap the
  platform at plain-HTTP-in-cluster forever and put registry credentials somewhere
  ADR-0010 says they must not go.
- **Synchronous push on upload.** Better UX — a component is deployable the moment
  upload returns. Rejected because a failed push then has no retry unless the queue
  exists anyway, and because it makes the wasm side stream the artifact outward through
  the one code path here with a known deadlock. A trigger on upload can be added on top
  of the queue later.
- **A separate `pusher` binary.** Cleaner separation than widening the applier's role.
  Rejected as a second process to deploy, supervise and give a secret to, for four HTTP
  calls that belong next to the loop that already polls the platform.
- **An external registry from the start** (ghcr.io, and there is already a
  `push-tempo-ghcr` recipe). Deferred: it adds an egress dependency to every cold start
  and an account to manage, to buy durability that `blob:store` already provides here.
  Revisit for multi-cluster, where it becomes the right answer.
- **htpasswd auth now.** Rejected above, with the reasoning recorded on the manifest so
  it is re-examined rather than inherited.
- **A registry crate** (`oci-client`). Rejected: a dependency tree for four calls, and
  it would own the media-type decisions that are the actual risk.
