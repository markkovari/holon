# ADR-0010 — Config is `wasi:config`; secrets never enter a manifest

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

Components in this repo are configured exclusively through `wasi:config` knobs —
that is the design property that lets one artifact run on a laptop, on NATS, and
on a cluster unchanged. `tools/gen-catalog.py` already extracts documented knobs
per component, and the operator delivers them as
`components[].localResources.config`, a flat string map.

The problem: **that map is plaintext in the manifest.** `vet-domain-v2.yaml`
currently ships

```yaml
config:
  master-key: "dmV0LWNsaW5pYy1kZW1vLW1hc3Rlci1rZXktMzJiISE="
  ticket-secret: ...
  cursor-secret: ...
```

which is fine for a demo whose secrets are demo secrets, and unacceptable for
tenants. `secretFrom` exists in the operator's documented vocabulary but appears
**only in a comment** in this repo — it has never been exercised.

## Decision

**Config and secrets are different things with different paths.**

- **Config** (non-sensitive knobs) is rendered into
  `components[].localResources.config` as today. The platform knows the legal keys
  for a component from its catalog surface plus the documented knob list, and
  **rejects unknown keys at save time** rather than letting a typo become a silent
  default.
- **Secrets** are stored by the platform in `secrets:vault` (AEAD envelope
  encryption, already in the catalog), rendered into a **k8s Secret in the tenant
  namespace**, and referenced from the workload by `secretFrom`. A secret value
  **never appears in a manifest, a deployment revision, an audit line, or a log**.
- A deployment revision (ADR-0004) stores config **by value** and secrets **by
  reference**. This keeps rollback meaningful without making revision history a
  secret store.
- Because `secretFrom` is unexercised here, **it must be proven on a real workload
  before any tenant secret exists.** Until it is, the platform accepts no secrets
  and the UI says so. Shipping a "secrets" field that lands in plaintext config
  would be worse than not having one.
- The platform's own credentials (the applier's shared secret, registry
  credentials, signing keys) are k8s Secrets read by native processes — never
  routed through `wasi:config`, which is a per-component view the guest can read
  in full.

## Consequences

- Two rendering paths in the renderer, and a `secretFrom` spike as an early task.
  If it turns out the installed operator doesn't support it, the fallback is a
  native-side injection at apply time — and that fallback needs designing then, not
  assumed now.
- Config validation gives tenants a real error ("`max-attempst` is not a knob of
  `ratelimit:guard`") instead of a component quietly running on defaults. This is
  cheap and high-value; the knob lists already exist.
- `secrets:vault` needs a master key, which is the platform's own bootstrap secret
  — a chicken-and-egg the applier resolves (it can read a k8s Secret; the wasm side
  gets the vault key through `wasi:config` at startup). That indirection is worth
  writing down because it is the one place the platform's own secret handling is
  not self-hosted.
- Per-tenant storage prefixes (ADR-0008) are *not* config: they are stamped
  isolation fields the tenant cannot see or set. Keeping them out of the config map
  prevents a tenant from "configuring" their bucket.

## Alternatives

- **Everything in `localResources.config`, secrets included.** Rejected: plaintext
  in an object any cluster reader can `get`, and in every stored revision.
- **An external secret manager (Vault/ESO) from the start.** Deferred: `secrets:vault`
  plus k8s Secrets is enough for slice 1 and keeps the dependency count down. The
  boundary is the same shape if it is swapped later.
- **Let tenants supply raw environment variables.** Rejected: components here read
  `wasi:config`, not env, and inventing a second configuration channel would break
  the "same artifact everywhere" property that makes the catalog portable.
