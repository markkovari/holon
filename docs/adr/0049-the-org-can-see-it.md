
# 0049 — The org can see it

Status: accepted. Builds ADR-0007's middle row, which was specified and never
implemented.

## The silent no-op

`component_publish` accepted `visibility: "org"` and wrote it to the row. `may_use`
seeded two rules — `catalog-own` and `catalog-public` — and no third. So an org-visible
component was evaluated by a rule set that had nothing to say about it, fell through
to the default deny, and behaved exactly like a private one.

The API said 200. Nobody could see it. That is worse than rejecting the value,
because the uploader believes they shared something and finds out otherwise only when
a colleague cannot find it.

It also meant the organisation model stopped at deployment: orgs owned apps
(ADR-0031) but not components, so "our team's shared store" was not expressible.

## The rule

```rust
policy::Rule {
    id: "catalog-org", priority: 7,
    conditions: [
        resource.visibility == "org",
        principal.org == resource.org,
    ],
}
```

Priority 7 sits between own (10) and public (5) — more specific than "anyone", less
than "mine", which is the order ADR-0007's table describes.

**A person can belong to several organisations**, and `policy:guard` compares single
values. Rather than reshape the rule engine around a list, `may_use` asks the same
question once per membership. Nobody is in enough orgs for that to matter, and the
alternative — a hand-rolled `if` next to a policy engine that exists to avoid exactly
that — is how the rule set stops being the description of who can see what.

Rows uploaded before this have no `org` and fall back to their uploader's tenant, so
their visibility is unchanged. Guessing anything else would silently widen who can
read old rows.

## Measured

Three users: `ada` owns `acme`, `bo` joins it, `zed` is an outsider.

```
ada uploads into acme, publishes visibility: org

who sees it in the market:   ada 1,  bo 1,  zed 0

who can DEPLOY it:
  bo    "has not been distributed yet — it has no content address"
  zed   "component `shared-store` is unknown or not visible to you"
```

The second half matters as much as the first. `bo` gets *past* visibility and stops at
a pipeline state; `zed` is refused as unknown. Seeing and using are the same decision
— both go through `may_use` — so a component that shows up in search is one you can
actually build with.

## The market endpoint

`GET /api/market?q=&iface=&org=` over the rows the catalogue already loads.

`?iface=` matches an **export**, because that is the question someone actually has:
*who can fill this gap in my graph*. It is the same match the 422 uses to suggest
candidates (ADR-0005), so the marketplace and the error message answer with the same
list rather than two implementations that drift.

**Visibility is applied first, before any filter.** A search that narrowed afterwards
would let a caller learn a private component exists from how many results came back.

`// ponytail:` linear scan over one page; add an index when the catalogue outgrows it.

## What this does not do

- **`public` is still 501.** It requires a signed digest and signing does not exist
  (ADR-0025). Private and org work; an unsigned public catalogue would be worse than
  none.
- **No `@version` in the key.** ADR-0007 is explicit that visibility is per *version*,
  precisely so a new version is not public by default. Today a row is one component
  and publishing it publishes all of it. That is the next data-model change, and it
  touches every catalogue key.
- **`deprecated` is stored and shown, and nothing enforces it.** Deployments naming a
  deprecated component still work, which is the intent (ADR-0007: deprecation, never
  deletion) — but nothing warns at deploy either, which is not.
