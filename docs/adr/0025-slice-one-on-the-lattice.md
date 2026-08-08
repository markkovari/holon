# ADR-0025 — Slice one, on the lattice: two boxes, one killed node

- **Status:** accepted
- **Date:** 2026-08-08
- **Supersedes:** [ADR-0011](0011-slice-one-scope.md) (slice-one scope)
- **In the spirit of:** [ADR-0018](0018-the-platform-deploys-a-running-app.md), which recorded the first live run and the bugs only a live run found

## The run

A MacBook and a Raspberry Pi 5 (`malna`, 4× A76, Debian bookworm, aarch64), joined only by
a NATS lattice. The Pi was given the `comp-host` binary and nothing else.

1. `comp login` / `comp component push` against the real `platform-domain` — the wasm
   control plane, on a `comp-host`, with no Kubernetes and no applier.
2. The reconciler distributed the artifact into the object store by digest.
3. **The Pi pulled 372 KB of wasm over NATS**, verified the hash, compiled it and served.
   Nothing was copied to it by hand.
4. `comp app create` + `comp app deploy` stored a manifest; the reconciler placed it.
5. Both boxes served the app, routed by `Host` header.
6. `pkill comp-host` on the Pi. Its inventory key expired, the Mac went from one replica
   to two, and it kept serving throughout.
7. The Pi restarted, **restored its instance from its own ledger before contacting NATS**,
   rejoined, and the fleet rebalanced to one replica each. The reconciler then went quiet.

That last property is the one that replaces the operator: a node that reboots during a
control-plane outage comes back serving, from its own disk, with no help from anyone. An
unreachable reconciler is not an instruction to stop.

## The bugs only two machines found

- **Deltas are not idempotent.** See ADR-0022. Six replicas of a two-replica app.
- **An `RwLock` deadlock.** `if let Some(x) = lock.read()…{ lock.write() }` holds the read
  guard across the block. Because the heartbeat reads the same table, the node stopped
  publishing inventory and had its work rescheduled out from under it — it took down both
  nodes at once. A loopback test never hit it because the second `start` never arrived.
- **A version mismatch nothing could have caught statically.** The host advertised
  `wasi:keyvalue/store@0.2.0-draft`; the manifest asked for `wasi:keyvalue/store`. Every
  deployment was permanently unschedulable, with a message that named both strings and
  still read as correct. Host interface matching is versionless now, in one place, with a
  test asserting no `@` ever creeps back in.

## Scope, and what is explicitly not in it

**In:** the manifest, node inventory, the pure diff, command emission, node-vanish,
hysteresis, capability partitioning, artifact distribution, the CLI.

**Out, and stated rather than quietly dropped:** cross-node WIT calls. Freely distributed
graphs are the architecture, but nothing in this repo has ever made a WIT call over a wire.
Graphs co-locate; a `linked` plug needs a fused artifact, and the host says so rather than
failing obscurely.

> **Corrected by [ADR-0028](0028-cross-node-calls-are-wrpc.md).** An earlier version of
> this paragraph justified the deferral with a hand-rolled `Val`↔JSON codec and "a hard
> ceiling — resource handles are indices into one process's table and cannot cross a node".
> Both halves were wrong: the codec should never have been written, because wRPC already
> specifies one, and the ceiling is narrower than claimed — wRPC fully supports `stream`
> and `future`, and encodes resources as opaque `list<u8>` whose meaning is
> application-specific. The deferral stands; the reasoning for it does not.

Also out: tenant config and secrets (ADR-0010 still promises both), `public` visibility and
signing (ADR-0007 rule 3), org-scoped catalogue visibility, per-version catalogue keys, a
UI, and `comp node ls` — which needs a NATS read the CLI does not yet do.
