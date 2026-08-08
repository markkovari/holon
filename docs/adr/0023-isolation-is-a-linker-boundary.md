# ADR-0023 — Isolation is a linker boundary, not a process boundary

- **Status:** accepted
- **Date:** 2026-08-08
- **Supersedes:** [ADR-0008](0008-isolation-is-stamped-never-authored.md), [ADR-0013](0013-unenforceable-capabilities-are-denied-by-omission.md), [ADR-0014](0014-an-application-owns-a-host.md), [ADR-0015](0015-a-bucket-name-is-not-a-boundary.md)
- **Answers:** [ADR-0012](0012-keyvalue-isolation-needs-a-cooperative-component.md), which measured the leak

## Context

ADR-0012 measured two tenants reading each other's records. The mechanism was exact: the
bucket was chosen by the guest's `store::open()` call, not by manifest config, so two
tenants running the same component reached the same store. ADR-0014 retreated — one host
pod per application, with a private NATS sidecar — and ADR-0019 priced that retreat at
roughly 24×, calling multi-tenant density blocked on an upstream ask.

It was never an upstream ask. It was a wasmCloud constraint. `comp-host` is our own
wasmtime binary: we own the linker, so the host knows which workload it is serving at
instantiation and can name the store itself.

## The law

ADR-0015 said a bucket name is not a boundary. That is a special case. The general rule,
which every capability here is an application of:

> A name is a real boundary iff **(1)** the name is chosen by host-side state the guest
> cannot write, and **(2)** the guest has no second path into the namespace.

wasmCloud failed both: `open(name)` let the guest choose, and the shared bus let any
component reach any bucket. Operationally it reduces to one greppable sentence, and it is
the thing to check before adding any capability:

> **No capability impl may use a guest-supplied string as a namespace selector.** A guest
> string may only be a lookup key into a host-side allow-list.

## Decision

`store::open(identifier)` looks `identifier` up in the instance's `Scope`. A hit yields
the store the platform assigned; a miss yields `no-such-store`.

A miss must **never** fall back to the default. A fallback would mean a guest naming its
neighbour's bucket gets *a* bucket instead of an error — the same class of bug wearing an
apology.

Three properties make this cheap:

- The guest string is a key into host state, so clause (1) holds by construction.
- It is default-deny.
- Every component in the catalogue hardcodes `open("default")`, so seeding that one name
  means **zero catalogue changes** — which is precisely what killed the fix ADR-0012
  proposed.

`BucketId`'s inner field is private to `host/src/tenant.rs`, and `kv.rs` accepts nothing
else. The compiler now enforces what ADR-0012's prose could not. The fix lives in `open()`
and on the resource, not in each method, so `atomics` and `batch` inherit it — there is no
sibling caller left holding a guest string.

### Which backends are actually boundaries

Clause (1) now holds everywhere; clause (2) discriminates.

| backend | verdict |
|---|---|
| memory | **Real.** No forgeable id, no second path into the heap. |
| sqlite | **Real.** A host-named bucket in a composite primary key, and the file is the host's. |
| redis | **A naming convention.** One keyspace, one credential, `SCAN` sees everything. Sufficient while the host is the only client; false the moment an ops script holds that credential. The real fix is one ACL user per tenant. |
| nats | **Restored in [ADR-0027](0027-a-spread-app-needs-a-shared-store.md).** The removal here was forced by a dependency conflict and justified after the fact by the per-account argument below — which is true but beside the point, since the host names the bucket on every backend. It is also the only backend where two replicas of one app share a store. |

> **Corrected by [ADR-0027](0027-a-spread-app-needs-a-shared-store.md).** This table
> conflates two properties. *Isolated* is about what a guest can reach and holds on every
> row above. *Shared* is about whether two replicas of one app see one store, and only
> `nats` and `redis` have it — spreading a stateful app across nodes with `memory` or
> `sqlite` silently gives each replica its own store. The reconciler now refuses that.

### Egress

`Host.hooks` was `[(); 0]` — upstream's zero-sized default, i.e. unrestricted outbound
HTTP. In a process shared by every tenant on a node that reaches the NATS bus, the host's
own listener, and every other node on the tailnet.

It is now default-deny with two independent checks: an allow-list on **names**, because
that is what an operator can reason about, and a deny-list on **resolved addresses**,
because a name check alone is satisfied by pointing an allow-listed name at
`169.254.169.254`. The address list covers loopback, RFC1918, link-local, v4-mapped v6,
and Tailscale's `100.64.0.0/10` — reaching that range is reaching the lattice.

## The claim is weaker than what it replaces, and that is the decision

ADR-0014/0018 gave every application its own pod and its own NATS. Two tenants writing the
same bucket name provably could not see each other, and the mechanism was an OS and
network boundary. **Collapsing to one process per node removes that mechanism.**
ADR-0008's release gate — "two tenants on one hostgroup, A provably cannot read B" — goes
from met back to unmet until the adversarial run below passes.

Stated plainly: **tenants share an OS process. The boundary is wasmtime's sandbox plus the
linker discipline above.** Specifically unmitigated:

- **Cross-tenant side channels.** One `Engine`, one code cache, one pooling allocator,
  tenants adjacent in an address space. Wasmtime zeroes reused slots and enables Spectre
  mitigations, and MPK helps where the kernel has it, but nobody has measured whether
  tenant A can observe tenant B's timing on this hardware. Container-per-app did not have
  this problem.
- **Per-tenant memory accounting.** ADR-0020 measured a host settling at 233 Mi and not
  giving it back. Shared, that residue is unattributable: `StoreLimits` caps a store's
  linear memory but nothing attributes pooling slots or code cache to a tenant. Quota and
  billing are unsolved and ADR-0020's "a quota counting idle memory would be wrong by 3×"
  gets worse, not better.

This was bought deliberately with the 24×. It is not a side effect of dropping Kubernetes,
and the honest retreat if the run below ever fails is process-per-tenant, which costs the
density and nothing else.

## The falsifying measurement

Two tenants, one `comp-host`, tenant A adversarial: `open()` over a dictionary of plausible
bucket names including B's; outgoing `wasi:http` to loopback, the metadata endpoint, the
node's own listener and the tailnet; and every subject it can construct.

**Pass = zero.** Publish it as `cross-tenant reads = 0 / 10^6 attempts` beside `rps` and
`p99` **from the same run**, so the density claim and the isolation claim are measured
together instead of in separate documents.

**Discharged by [ADR-0026](0026-the-adversarial-run.md)**, which ran it: two tenants in one
process on one sqlite file, a hostile component sweeping a dictionary of leak-shaped bucket
names and dialling the bus, the metadata endpoint and the tailnet — zero foreign opens,
zero keys read, zero lateral connections, taken at 10.5k rps under load. Note the `10^6`
framing above is superseded there: a dictionary of the shapes a real leak takes is a
stronger instrument than a large number of random ones.

The two unmitigated risks named earlier in this ADR — side channels and per-tenant memory
accounting — are NOT addressed by that run and remain open.
