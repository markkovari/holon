# ADR-0031 — An organisation owns a deployment, and a person can be in several

- **Status:** accepted
- **Date:** 2026-08-08
- **Extends:** [ADR-0009](0009-identity-reuses-auth-guard.md), whose single tenant is right for authentication and wrong for ownership
- **Widens:** [ADR-0023](0023-isolation-is-a-linker-boundary.md)'s isolation unit from tenant to org

## Context

`auth-guard` gives one identity with **one** tenant and roles scoped to it. That is
correct for authentication and insufficient for ownership: a person contracting for three
companies has one identity and three separate sets of things they may touch, and three
people at one company must reach the same deployments.

Ownership was `doc["tenant"] == principal.tenant`, with the tenant derived from the email
local part. That makes ownership a property of an address.

## Decision

**Organisations sit above auth-guard.** It still answers *who are you*; orgs answer *on
whose behalf, right now*. Deployments, catalogue rows and — critically — the storage bucket
a running instance gets are keyed by org id.

- **Everyone gets a solo org on registration**, named after their personal tenant. Without
  it, every existing call would need an explicit org and a person who never joins a company
  would have nowhere to deploy. It also means "org" is never an optional concept with two
  code paths.
- **Registering into an existing org name does not join it.** Otherwise registration would
  be an access-control bypass for anyone who can guess a tenant name. They need an invite.
- **Membership is by single-use invite code**, not by email. Sending mail is a subsystem
  this needs none of: an owner mints a code, hands it over however they like, the holder
  redeems it, and the invite is deleted on success so a leaked used code is worth nothing.
  The code is the record store's own id — unguessable and already unique.
  *(ponytail: no email; add invitations-by-address when there is a mail path to put them on.)*
- **Roles are ordered** — `viewer < member < owner` — so a check is a comparison rather
  than a set of equality tests with one call site forgotten when a role is added.
- **The last owner cannot leave.** An org with no owner can never have its membership
  changed again, which is unrecoverable without a support ticket.
- **A non-member gets 404, not 403.** Whether an org exists is itself information.

`orgs::acting()` returns the org *and* the caller's role together, so a handler cannot
check membership without also checking permission — there is one call and it answers both.
`owned_deployment` takes the required role as an argument, which is why adding it produced
a compile error at every call site rather than a silent default:

| action | needs | why |
|---|---|---|
| read a deployment, list members | viewer | |
| save/deploy, upload a component | member | it runs code on the org's behalf |
| delete a deployment | owner | destroys its storage permanently (ADR-0016) |
| invite, remove someone else | owner | widens who can run code |

## Isolation

`manifest::env_for(org, app)` replaces `env_for(tenant, app)`, so the storage bucket and
the ingress hostname are derived from the **org**. ADR-0023's property is unchanged and now
wider: two orgs cannot see each other's data for exactly the reason two tenants could not,
because the host still names the bucket from a control-plane record the guest cannot write.

Quota moved with it — three people in one company share one deployment allowance, and one
person in three companies does not carry theirs between them.

## The measurement

Three people register; each gets a solo org. Ada creates `acme-corp` and invites Grace, who
joins as a member and deploys into it.

```
ada:    ada(owner)      grace: grace(owner)     linus: linus(owner)
acme-corp members:  usr_d236…(owner)  usr_4009…(member)

grace deploys ->  revision 1 of `shop` saved
ada (same org, never touched it) ->  shop, revision 1, saved
linus (not a member)            ->  no deployments
linus reads it directly         ->  404 not_found
linus deploys into acme-corp    ->  404 no organisation `acme-corp` that you belong to

storage:  acme-corp / shop -> env app-acme-corp-shop
ingress:  shop.acme-corp.apps.local
```

The last two lines are the point: the bucket is named after the **org**, not after Grace
who typed the command.

## What is still wrong

- **Old rows fall back to `tenant`.** `owned_deployment` reads `org` and falls back to
  `tenant` so a deployment written before this ADR stays reachable by its author. That
  fallback is a second code path and should be removed once nothing predates orgs.
- **`may_use` on the catalogue still compares tenants.** Component visibility is unchanged,
  so `org`-visible components (ADR-0007's middle row) remain unimplemented — an org's
  members do not yet automatically see each other's uploads.
- **No org-level plan.** `plan_of(org)` reads the same `ACCOUNTS` collection keyed by name,
  so an org created today gets defaults and there is no way to price one differently.
- **Nobody can rename or delete an org**, and a solo org cannot be cleaned up.
