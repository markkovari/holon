# ADR-0007 — Component visibility: private, org, public — and what public costs

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

The product requirement is that a tenant can "upload their components or use the
built-in ones or others' public ones". That last clause is the interesting one: it
means one tenant's deployment can run bytes another tenant published. Every
supply-chain problem in the industry lives in that sentence.

What we have to build on: `auth-guard` is already tenant-scoped (every function
takes a `tenant`, and `principal` carries one), roles are resolved server-side from
the RBAC store and never trusted from a token, and `policy:guard` exists precisely
for row-level "does this principal own THIS row" decisions with rules stored in KV
and a default of deny.

What we do not have: any signing. `cosign` appears in `PLATFORM.md` phase 1 as a
plan and nowhere in the repo as a fact. The registry has no authentication at all.

## Decision

Three visibility levels on a component **version** (not on a name):

| visibility | who may reference the digest | who sees it in the catalog |
|---|---|---|
| `private` | the owning tenant only | the owning tenant |
| `org` | any tenant in the same organisation | that organisation |
| `public` | any tenant | everyone |

Rules that make this safe enough to ship:

1. **Visibility is per version, and only ever widens by an explicit act.** Making
   `v3` public does not make `v4` public. There is no "publish the latest".
2. **Referencing is by digest** (ADR-0006). A consumer's deployment pins the exact
   bytes it was built against, so a publisher cannot change what a consumer runs
   — the worst they can do is stop offering new versions.
3. **`public` requires a signature.** A version cannot become public unless its
   digest is signed by the publisher's key, and the platform verifies the
   signature before allowing the transition. This is the one place signing is
   load-bearing, so it is the one place we build it first. Private and org
   versions do not require it.
4. **Deprecation, not deletion.** A public version's digest must remain resolvable
   while any deployment references it; publishers may mark a version deprecated
   (a UI warning for consumers) but cannot pull the bytes out from under a running
   deployment. Reference-counted, and enforced on delete.
5. **Public means the surface is public too.** The WIT surface, size, instance
   count and provenance (who published, when) are visible to any tenant — that is
   how a consumer decides whether to trust it. Config *values* never are
   (ADR-0010).
6. **A public component gets no extra trust at runtime.** It is stamped with the
   consuming tenant's isolation exactly like the tenant's own code (ADR-0008): the
   consumer's buckets, the consumer's egress allow-list. Running someone's
   component must never mean running it with their access, nor with more than
   yours.

Ownership and access checks are `policy:guard` rules over `{principal.tenant,
principal.org, resource.owner, resource.visibility}`, not hand-coded conditionals
at each route — the vet-clinic's 17 hand-coded ownership sites are the anti-pattern
`policy:guard` was extracted to fix.

## Consequences

- **Signing must exist before the first public component**, which puts a cosign
  (or equivalent) verify step on slice 1's critical path if public sharing is in
  slice 1. If it isn't, `public` stays disabled and the feature ships with
  `private` + `org` only — an honest partial rather than an unsigned free-for-all.
- The registry needs authentication so that "private" is true at the storage layer
  and not merely in our catalog rows. Today it has none (ADR-0006 already makes
  this a prerequisite).
- Reference counting on versions is now a data-model requirement: deployments hold
  digests, and delete paths must consult them.
- A public catalog invites the obvious abuse (someone publishes something hostile
  and appealing). Mitigations we accept for slice 1: signing, provenance on
  display, the surface being visible before use, and the isolation stamp meaning a
  hostile component runs with the *consumer's* nothing. Mitigations explicitly
  deferred: review queues, reputation, scanning.
- "Built-in" components are just `public` versions published by a platform-owned
  tenant. No separate code path, and the same signature requirement applies to us.

## Alternatives

- **Private only.** Rejected: "use others' public ones" is a stated requirement,
  and a catalog nobody can share is the studio with a login.
- **Visibility on the component name rather than the version.** Rejected: it makes
  a future version public by default, which is the failure mode where someone
  accidentally publishes a version with a credential baked in.
- **Fork-on-use (copy the bytes into the consumer's namespace).** Considered — it
  gives perfect immutability and no reference counting. Rejected because it
  destroys the update story: a consumer could never learn that a fixed version
  exists, and storage multiplies by consumers.
- **No signature requirement, rely on provenance display.** Rejected: provenance
  without signing is a claim by whoever controls the row. The digest is signable
  and the check is cheap; do it at the boundary where bytes cross a trust line.
