# ADR-0012 — Per-tenant keyvalue isolation needs a cooperative component

- **Status:** accepted
- **Date:** 2026-07-28
- **Supersedes:** the storage half of [ADR-0008](0008-isolation-is-stamped-never-authored.md)

## Context

ADR-0008 said the platform stamps a per-tenant bucket onto every `wasi:keyvalue`
`hostInterfaces` entry, and flagged that the mechanism had "only ever been exercised
for blobstore" and was therefore **unproven**. It also set a release gate: no second
tenant until an adversarial test shows tenant A cannot read tenant B's storage.

The gate was tested for real, on the cluster, and **the adversarial test failed.**

Two tenants (`mark`, `eve`), each in their own namespace, each running the same
`mesh` app scheduled onto the shared host in `jobs`. Tenant A made one guarded call,
which stored a circuit record under the key `live`. Then:

```
MARK  /api/circuit/live : window_start_ms = 1785210803454
EVE   /api/circuit/live : window_start_ms = 1785210803454   ← the same record
MARK  /api/circuits keys: ["live"]
EVE   /api/circuits keys: ["live"]                          ← A's key, in B's app
```

The cause is not a bug in the stamp; it is that the stamp was **read by nothing**.
The operator's CRD documents how a bucket is actually chosen:

> `name` — "Name uniquely identifies this interface instance when multiple entries
> share the same namespace+package. **Components use this name as the identifier
> parameter in resource-opening functions (e.g., `store::open(name)`).**"

So the bucket is picked by the *guest*, via the identifier it passes to `open()`, and
matched against a `hostInterfaces` entry's `name`. Every storage-backed capability in
this catalog hardcodes it — `components/record-store/src/lib.rs:47`:

```rust
const BUCKET: &str = "default";
kv::open(BUCKET)
```

A `config: { bucket: t-mark }` key on the entry is not part of that contract. It was
emitted for two real deployments, looked exactly like isolation in the rendered YAML,
and isolated nothing.

## Decision

**Storage isolation is cooperative: the platform chooses the name, and the component
must open it.** Three parts.

1. **Stop emitting a `bucket` config key on `wasi:keyvalue` entries.** A field that
   nothing reads but looks like a boundary is worse than an absent one — it is the
   reason this shipped through two deploys and a passing test suite. The renderer now
   emits a comment saying storage is not isolated, pointing here.
2. **The gate is enforced in code, not in a document.** `platform-domain` refuses a
   save for any tenant other than the first unless `allow-multi-tenant=true` is
   configured, and the refusal names this ADR. ADR-0008's gate was prose; prose does
   not stop a deploy.
3. **The fix, when we take it:** a storage-backed capability reads its bucket
   identifier from `wasi:config` (falling back to `"default"`), the platform stamps
   `hostInterfaces[].name: t-<tenant>` plus the matching component config, and the
   adversarial test above becomes a permanent test. This is a change to
   `records:store` and its siblings — backward-compatible via the fallback, but it
   touches the catalog rather than the platform, which is why it is a decision and
   not a patch.

`wasi:blobstore` keeps its `buckets:` allow-list: that one has a working precedent
(`bench-suite-v2.yaml`) and is enforced by the host's plugin rather than chosen by
the guest.

## Consequences

- **The platform is single-tenant until a catalog change lands.** Everything
  tenant-shaped — namespaces, quotas, policy, the ownership model — works and is
  exercised; the storage boundary is the one thing missing, and it is the one that
  matters most.
- Any capability that wants per-tenant storage now has a requirement on it, not just
  on the platform. That list is longer than `records:store`: `cache:store`,
  `session:store`, `blob:store`, `search:index`, `outbox`, `event:bus`, `quota`,
  `lock:mutex` — everything importing `wasi:keyvalue`.
- The alternative shape, one host *environment* per tenant, remains available and is
  now more attractive than ADR-0008 assumed: it needs no catalog change, and
  `template.spec.environment` already selects a host across namespaces. It costs a
  host per tenant, which is the density bet PLATFORM.md declined — but "declined" was
  priced against a bucket stamp that does not exist.
- Namespace-level `NetworkPolicy` is also weaker than ADR-0002 implied for
  shared-host workloads: the component runs in a pod in the *host's* namespace, so a
  policy in the tenant's namespace selects nothing. Egress control rests entirely on
  `allowedHosts`. Worth its own ADR if we keep shared hosts.
- The upstream item to watch is unchanged (#5051, multi-backend `hostInterfaces`),
  but it is now clear what it would buy: a host-side mapping from workload to
  backend, which is what makes the boundary non-cooperative.

## Alternatives

- **Keep stamping `config: { bucket: … }` and hope.** Rejected on evidence: two
  tenants read the same record through it.
- **Have the platform rewrite each component so it opens a tenant bucket.** Rejected:
  rewriting a tenant's bytes to change behaviour is exactly what a platform must not
  do, and it would break the digest pinning that makes re-apply reproducible.
- **A host per tenant now.** Not rejected — deferred. It is the fastest route to a
  real second tenant and needs no catalog change; it should be priced properly before
  the cooperative-bucket work starts, because it may simply be the better trade.
- **Ship multi-tenancy with the boundary documented as absent.** Rejected. The
  adversarial test is the product (ADR-0008), and a platform whose tenants read each
  other's records is not a platform.
