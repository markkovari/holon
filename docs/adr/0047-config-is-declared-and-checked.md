# 0047 — Config is declared by the uploader and checked at save

Status: accepted. Delivers the error ADR-0010 promised and never built.

## What was there

`manifest.rs` said it plainly:

```rust
// Tenant config is not wired yet (ADR-0010 promises it); the field
// exists so a node's start command has one shape either way.
"config": {},
```

So every deployment shipped an empty config map. A component reading
`wasi:config/store` got nothing, and there was no way to give it anything — which
makes half of the original goal ("deployed only when the configurations are set up
correctly") unreachable, because there were no configurations.

## Declared by the uploader

```
POST /api/components?id=gate&config=grace-period-secs!,retries
```

A trailing `!` marks a key required. The uploader is the only party who can answer
this: they own the component and can see what it asks `wasi:config` for. The platform
cannot infer it — a component's WIT says it imports `wasi:config/store`, not which
keys it will look up — and a platform that guessed would either reject valid config
or wave typos through.

Stored on the catalogue row and returned by `GET /api/components`, so a caller can
render a form from the declaration instead of discovering keys from a 422.

## Checked at save, with the message ADR-0010 wrote down

Measured against a live control plane:

```
a typo in a config key
    `gate` has no config key `grace-period-sec` — it takes ["grace-period-secs", "retries"]

a missing required key
    `gate` requires config `grace-period-secs`, which is not set

correct config
    accepted
```

The first is the whole point. "Rejected" is useless; naming the key you wrote *and
the ones that exist* is the difference between a two-minute fix and a support
ticket. It costs one comparison against data already loaded.

**A component that declares nothing accepts nothing.** Silence means "reads no
config", not "reads anything" — deny by omission, as everywhere else here (ADR-0013).
The message says so explicitly, because an empty list of legal keys otherwise reads
as a bug in the platform.

Refused at save, before anything is composed or staged: a config error belongs to
the author and costs nothing to find there, while the same mistake reaching a node
becomes a component that starts and then fails in front of a user.

## The case fusing creates

A fused artifact is **one** component with **one** `wasi:config/store`. Two
components in the graph that both set `token` are not two settings once `wac` has
composed them — there is no "whose" left. Same value: fine, merged. Different values:
refused, naming both components and suggesting `linked`.

This has no equivalent in the linked strategy, where each instance keeps its own
config, and it is the sort of difference between the two strategies that is invisible
until it silently picks one value.

## What is not done

- **Secrets are still 501.** `secrets:` in a manifest is by reference only (ADR-0010)
  and nothing resolves those references yet. Config and secrets are deliberately
  different paths — a value that may be logged and a value that may not.
- **No types or defaults.** Every value is a string, because that is what
  `wasi:config/store` hands the guest; declaring `retries: int` would be a promise
  the runtime does not keep.
- **No per-environment config.** One set of values per deployment. Staging and
  production are two deployments today.
