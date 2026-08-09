# 0044 — Subjects carry a version

Status: accepted. Acts on the failure ADR-0039 watched happen to someone else.

## Why now

Subjects were `comp.<lattice>.cmd.<node>.<verb>` with no version. ADR-0036 noted the
gap; ADR-0039 turned it from a theoretical worry into an observed failure, on
wasmCloud rather than here:

> A `wash` host at 2.3.0 against a 2.5.2 control plane placed workloads and then never
> ran them: `Config`, `HostSelection` and `Placement` all True, `Sync` stuck at
> `WORKLOAD_STATE_NOT_FOUND`. Nothing in the operator or host logs named a version
> mismatch.

It cost most of an afternoon to find, on a project that *does* version its control
subjects. We had the same exposure with none of the mitigation.

**Mixed versions are the normal state**, not an edge case: a fleet is upgraded one
machine at a time, and this session shipped a stale binary to malna once already
(ADR-0034) and silently dropped two nodes from a benchmark.

## The change

`wire::V = "v1"`, in every subject:

```
comp.v1.<lattice>.cmd.<node>.<verb>
comp.v1.<lattice>.rpc.<tenant>/<app>/<component>
```

The data plane is versioned separately from the control plane, through the same
constant but a distinct function, because the two can move independently and a shared
literal is how they get changed together by accident.

**What this buys is a better failure, not the absence of one.** An old node does not
receive commands from a new reconciler at all: it goes quiet, its inventory expires,
and the fleet treats it as absent — a state this platform already handles correctly
(ADR-0022) and which is visible from outside. "Present but silently misparsing" is
neither handled nor visible.

Bump `V` when a command or inventory entry changes shape incompatibly. Not for
additive fields — serde already tolerates those, and a version bump that stops a
rollout for a new optional field is a version bump nobody will do next time.

## Notes

- Two existing tests failed on the change, which is exactly what they were for: both
  pin the literal subject string, so a rename cannot happen quietly in one binary and
  not the other. They were updated, not deleted.
- The end-to-end proof is an activation round trip — ingress → reconciler → node start
  → serve — which crosses both subject families and only works if every binary agrees.
- **This does not migrate anything.** A fleet mid-upgrade has old nodes going absent
  until they are restarted on the new binary. That is the intended behaviour and it is
  still a real operational cost — worth knowing before an upgrade, not during one.
