# jco-policy

Exercise the `policy:guard@0.1.0` component **in-process** via
[jco](https://github.com/bytecodealliance/jco) — no host runtime required.

## What it is

`policy:guard` is a declarative **attribute / row-level** authorization engine
(ABAC). It complements `auth-guard`'s coarse RBAC: where RBAC asks "does this
role have this permission?", `policy:guard` asks the finer-grained question
"does _this principal_ have this permission on _this specific row_?".

A decision matches rules against attributes of the **principal** (`principal.*`)
and the **resource** (`resource.*`). The model is:

- **default-deny** — if no rule matches, the answer is `false` (empty `ruleId`).
- **deny-overrides at equal priority** — when an `allow` and a `deny` both match
  at the same priority, `deny` wins.

### The canonical example: vet-clinic appointments

> An owner may cancel **their own** appointment.

```ts
{ id: "owner-cancel", action: "cancel", effect: "allow", priority: 10,
  conditions: [{ left: "resource.owner", op: "eq", right: "principal.subject" }] }
```

The condition's `right`-hand side `principal.subject` is resolved against the
principal's attributes at decision time, so the rule is true only when the
appointment's `owner` equals the caller's `subject`. Layer a wildcard staff rule
(`action: "*"`, `role in-list doctor,admin`) and a `deny` on completed
appointments, and you have the full clinic policy in three rows.

## API (camelCase JS bindings)

- `policy.setRules(domain, rules)` — install rules (throws `invalid-rule` on a bad condition)
- `policy.getRules(domain)` — read them back
- `policy.can(domain, action, principal, targetAttrs)` → `{ allowed, ruleId, reason }`
- `policy.enforce(domain, action, principal, targetAttrs)` → `boolean`

Enum values map to kebab strings: `op ∈ 'eq' | 'ne' | 'in-list' | 'lt' | 'gt' | 'has'`,
`effect ∈ 'allow' | 'deny'`.

## The keyvalue shim is swappable

The component imports `wasi:keyvalue/store`. Here it's a trivial in-memory `Map`
([`src/keyvalue-shim.js`](../src/keyvalue-shim.js), shared with the other jco
examples). Point the `--map` at a redis / sqlite / NATS shim and the persistence
becomes real — the component neither knows nor cares.

## Run

```bash
npm install
npm test        # transpiles policy_guard.wasm -> gen/, then runs test/policy.test.ts
```
