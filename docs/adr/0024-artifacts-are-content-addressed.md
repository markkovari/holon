# ADR-0024 — An artifact is its digest, and the object store is a cache

- **Status:** accepted
- **Date:** 2026-08-08
- **Supersedes:** [ADR-0017](0017-the-applier-pushes-and-the-registry-is-a-cache.md) (the applier pushes, the registry is a cache)
- **Keeps:** [ADR-0006](0006-artifacts-are-digest-pinned-oci.md)'s digest pinning, intact

## Context

ADR-0006 made the digest the identity and ADR-0017 made the applier push bytes to an OCI
registry, driven by "has no `oci_ref`" rather than a queue. The identity rule was right.
The registry was scaffolding for Kubernetes: the operator pulled images, so there had to
be something to pull from.

## Decision

**Artifacts move through a JetStream object store, keyed by their own sha256.** A node
fetches by digest, verifies the bytes hash to the name it fetched them under, and only
then compiles. The object store is not a trust boundary; the digest is.

The catalogue stores a bare `sha256:…` rather than `registry/repo@sha256:…`. A reference
naming a registry would name something no node can reach, and would give the same bytes
two identities.

OCI leaves the runtime path entirely. NATS is already mandatory for control and
distribution, so a second mechanism with its own auth, TLS and registry pod buys nothing.

## What survives

`push_artifact` / `oci_shape` / `upload_blob` and both their tests move to
`reconciler/src/oci.rs` behind `--oci-mirror`, **off by default**. They were proven against
a real registry, with a test asserting media-type parity with `wkg`; deleting a working
push path saves nothing and costs a rewrite the first time someone wants `wkg oci pull` to
interoperate.

The distribution queue is unchanged, including the property that matters: *pending* is
still derived from *has no digest*, so a crash costs a repeated upload and never a wrong
one, and there is no queue for anyone to keep in step.

The catalogue's `sha256` prefix check on fetch also survives. It is a corruption check on
the transfer, not an authenticity check, and it caught a real mangled transfer on its
first run — which is the argument for having written it.
