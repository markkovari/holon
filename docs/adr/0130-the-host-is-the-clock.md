# ADR-0130 — The host is the clock

*An app whose timers are pull needs something to pull for it. That was a native daemon beside the
host; the host already runs the app, so it pumps it itself.*

**Status: accepted.** Supersedes [ADR-0096](0096-a-pull-contract-needs-a-relay.md)'s native relay;
its reasoning about pull contracts stands.

## The problem

`sched:timer`, `event:bus` and `cron:expr` are deliberately pull (ADR-0096): pure WASI, so they run
on any host. Something outside must `POST` the app's pump, and that was `comp-relay`, a seventh
native thing, under ADR-0095's "a component has no background".

The relay was the wrong owner of that background. It needed its own unit beside each app, an
allow-list of target URLs (a poker aimed at an arbitrary host is a confused deputy with a timer),
a lease so two copies would not race a consumer group's offsets, and a NATS watch for latency.
Everything it knew about its target, the host that serves the app already knows.

## The decision

- **comp-host pumps the app it serves.** `--pump <path> --pump-every <secs>`: the host `POST`s the
  path to its own listener on loopback, every interval, the first at once (the catch-up after
  downtime). The request goes through the same door as any other, so the app's limits, scope and
  timeout apply unchanged. There is no target to configure, so nothing for it to be aimed at.
- **An app keeps declaring `[triggers]`.** Self-hosting renders them into the app's own unit as
  those two flags; no second unit is installed.
- **`comp-relay` is deleted.** The sweep was its contract and is kept. Its bus watch, the latency
  path that pumped early on a publish, is not carried over: ADR-0096 already said push never
  replaces the sweep, so an app loses at most one interval of latency, never a delivery.

## What is not built yet

**A lattice.** A manifest cannot carry triggers today, so nothing on a lattice was pumped by the
relay either unless an operator ran one by hand. When manifests gain triggers, the start command
carries them and the node pumps its instance under a per-instance lease
(`lattice/src/lease.rs`), the relay's guard against two pumpers racing a consumer group. Until
then `--pump` refuses to start beside `--lattice-nats`.

## Consequences

- One fewer native binary; a self-hosted app with triggers is one unit, not two.
- The pump's path is checked when the app spec is (`/` followed by letters, digits and `/_.-`),
  since it is interpolated into the unit's `ExecStart=`.
- A box upgraded from a relay keeps a stale `comp-<app>-relay.service` until it is removed; it
  pokes a port the app still answers, so it is redundant rather than wrong.
