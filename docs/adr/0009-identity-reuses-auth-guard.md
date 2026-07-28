# ADR-0009 — Sign-in reuses `auth-guard`; OIDC is a later swap

- **Status:** accepted
- **Date:** 2026-07-27
- **Supersedes:** —

## Context

People sign in, belong to an organisation, and own deployments. The platform needs
accounts, sessions, roles and row-level ownership.

`auth-guard` already is this, and was built multi-tenant from the start: every
function takes an explicit `tenant`, `principal` carries `{subject, tenant, roles,
scopes, expires-at}`, and — the property that matters most here — **roles are
resolved server-side from the RBAC store and never read from the token**, so a
token cannot grant itself a role. It also already has an `oidc` interface and a
documented claim convention for deriving a tenant from an external issuer, and
`examples/idp-oidc` exists with Zitadel/Ory seed scripts.

`policy:guard` covers what RBAC cannot: "does this principal own THIS deployment",
as rules in KV with a default of deny, rather than the 17 hand-coded ownership
checks the vet-clinic accumulated.

## Decision

**The platform's identity is `auth-guard`, composed into `platform-domain`, with
`policy:guard` for ownership.** No new identity component.

- `tenant` in `auth-guard` **is** the platform's tenant, so the identity model and
  the isolation model (ADR-0008) key on the same string. One notion of tenant, not
  two that must be kept in sync.
- Roles are coarse and platform-wide: `owner`, `member`, `viewer` per tenant.
  Anything finer is a `policy:guard` rule, not a role.
- Every mutating platform action is a `policy:guard` decision over
  `{principal.tenant, principal.roles, resource.owner, resource.tenant,
  resource.visibility}` — including the cross-tenant read that ADR-0007's `public`
  visibility permits, which is a rule, not an exception in code.
- Sessions are `auth-guard` sessions. The UI is same-origin with the API, so a
  session token is enough; no separate API-token concept in slice 1.
- **OIDC is deferred, not designed around.** `auth-guard`'s `authorizer` already
  accepts either an opaque session token or a JWS and detects which by prefix, so
  adding an external IdP later changes configuration and a claim mapping — not the
  platform's code. The tenant-from-claims convention is already documented.

## Consequences

- Sign-in works on day one with no external dependency, which keeps the platform
  runnable on a laptop (`--kv memory`) for development and demos.
- Password handling is `auth-guard`'s (argon2, lockout, TOTP available), so the
  platform inherits a reviewed implementation rather than growing a second one.
- `platform-domain`'s import list grows: `auth:identity` (composed `auth-guard`),
  `policy:guard`, `records:store`, `quota:meter`, `blob:store`, `wit:reflect`.
  That is a large graph for one component — and a good forcing function, since it
  is exactly the kind of app the studio exists to compose. **The platform should be
  deployable by the platform.**
- Because `auth-guard` resolves roles server-side, revoking access takes effect on
  the next request without token revocation machinery.
- A tenant is created by the platform, not self-served, until ADR-0008's
  adversarial gate passes. Sign-up therefore starts as invite-only — a product
  consequence of a security decision, and the honest sequencing.

## Alternatives

- **External OIDC (Zitadel/Ory) from the start.** Rejected for slice 1: it adds a
  service to operate before there is a user, and `auth-guard` already speaks OIDC
  when it is wanted. The switch is configuration.
- **A new platform-specific identity component.** Rejected: it would duplicate the
  one component in this repo that is already tenant-aware and role-server-side, and
  it would create two tenant notions to reconcile.
- **`auth-guard` for users, a separate service-token system for API access.**
  Deferred, not rejected — needed the moment a CI pipeline wants to deploy. It is
  additive (a token kind `auth-guard` already models), so it does not need
  deciding now.
