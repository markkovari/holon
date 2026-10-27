# ADR-0122 — S3-backed Git via walgit for component lineage and artifact handoff

*Autonomous component generation needs parallel branch exploration without git locks, and a green gate needs to hand off to standard Git without forge tokens or a GitHub dependency.*

**Status: accepted, and proven locally against rustfs.**

## The problem

Generational component creation (ADR-0081, ADR-0082, ADR-0089) tests candidate implementations of a WIT contract across generations. When swarms of agents explore candidate variants in parallel, standard Git fails them in three ways:

1. **Locks and concurrency**: `.git/index.lock` serializes writers on a checkout; parallel exploration either serializes or requires duplicating workspaces across NVMe disk.
2. **Forge dependencies**: Landing a winning candidate previously required `git:forge` (ADR-0082, `components/github-forge`) speaking to GitHub over HTTP. This requires Personal Access Tokens resolved from Vault (ADR-0010, ADR-0050), network egress, and hits GitHub API rate limits (5,000 requests/hr).
3. **Storage bifurcation**: Source trees live in Git, while compiled `.wasm` artifacts live in object storage or NATS (ADR-0006, ADR-0024, ADR-0098), requiring two distinct storage planes and reconciliation.

## The decision

1. **Walgit over S3-compatible storage for Git handoff and lineage.** We adopt walgit (`tobi/walgit`), a database-free Git server that treats an S3 bucket as the repository via write-ahead logging (WAL) and Compare-And-Swap (CAS) manifests (`manifest.pb`). Local disk is purely an ephemeral cache.
2. **S3 as a first-class `BlobStore` for `holon-vcs`.** In `crates/holon-vcs`, `BlobStore` gains an S3 adapter (`S3Blobs`) alongside `NatsBlobs`. Source code blobs and compiled `.wasm` artifacts share the same content-addressed S3 bucket (RustFS locally, Cloudflare R2 or Backblaze B2 in production).
3. **Internal agent loop stays lock-free; walgit is the external Git bridge.**
   - Agents edit symbol-level patch graphs in `comp-vcs` without branches or PRs (ADR-0099).
   - Gates (`comp-checks`, `components/gates.sh`) evaluate materialized snapshots.
   - When a candidate passes all required checks, `comp-vcs` computes the canonical `git-tree` SHA-1 (already implemented in `crates/holon-vcs/src/git.rs`) and pushes the commit directly to walgit via Smart HTTP or direct bucket WAL write.
   - Humans and external CI clone directly from walgit via standard `git clone` or inspect diffs in walgit's web UI without GitHub.
4. **Local compose profile.** `infra/compose.yaml` gains a `git` profile running walgit on `:8080` backed by the existing `rustfs` container on `:9000`.

## What it does not do

- Walgit does not replace Holon VCS's internal patch engine: Walgit serves standard Git snapshots and packfiles; it does not perform AST symbol-level commutation or conflicts-as-data (ADR-0099).
- Multi-region replication is left to the S3 bucket's underlying storage replication (e.g., Cloudflare R2 or RustFS clustering).
- Authentication for local dev is `mode = "none"`; production deployments configure OIDC (`server.auth.mode = "oidc"`).
