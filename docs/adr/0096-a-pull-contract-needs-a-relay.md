# ADR-0096 — A pull contract needs a relay, and the relay is native

*Three capabilities describe work that happens later. None of them could make it
happen, and nothing in the tree admitted that in one place.*

**Status: accepted.** Adds the sixth native thing, under the rule ADR-0095 wrote
for exactly this.

## The gap

`sched:timer@0.1.0` is a durable job store with recurrence and leases.
`event:bus@0.1.0` is a durable log with per-consumer-group offsets.
`cron:expr@0.1.0` parses a cron string and computes fire times. All three are
deliberately **pull**, and each says so in its own header — the timer's is
explicit: *"the component owns the when; the relay owns the what."*

Pull is the right choice. It is what keeps all three pure WASI, so the same
component runs on `comp-host`, on jco and on a wasmCloud host with no host
plugin to arrange. The cost is that something outside must poke the app, and
until now nothing in this repository was that something:

- on Kubernetes it was a **curl-loop Deployment** (`bench/ESHOP-BENCH.md`);
- on the native lane it was **a person refreshing a browser tab** —
  `docs/apps/ESHOP.md` says the storefront page pumps because that lane "has no
  messaging plugin";
- `comp-goald` has `--once` "for a cron", and no cron was ever written.

So the platform had timers that never fired unless a human or a YAML loop was
watching. A deployed `saga-domain` with a failing leg sat at `running` forever.

## The decision

**One native daemon, `comp-relay`, that POSTs an endpoint on a schedule.**

That is the whole of it. `/internal/pump` is already the convention —
`saga-domain` and the four `eshop-*` services all export it — and each app drains
its own timers and consumer groups behind it. So the relay speaks no WIT, holds no
bindings, and does not know what a job is.

### Why it is allowed to be native (ADR-0095's three questions)

1. **Does it need something WASI does not give a guest?** Yes: a clock that runs
   when no request is in flight, and a held subscription. A `wasm32-wasip2`
   component exists between an incoming request and its response and has no
   background at all. Same reason `reconciler/` is native.
2. **Is it the smallest it could be?** It decides only *when* to poke.
   Eligibility, recurrence and leasing stay in `scheduler-timer`; offsets and
   at-least-once stay in `event-bus`; the work stays in the app. It carries no
   business logic, which is the property that keeps it outside the isolation
   boundary honestly.
3. **Does it answer a contract a component could have answered?** It *drives*
   them rather than replacing them. `sched:timer` and `event:bus` stay in WIT
   unchanged, and what the relay actually speaks is the app's own
   `wasi:http/incoming-handler`. **No app changes to gain a trigger**, which is
   the test that this is a deployment concern and not a redesign.

## Two paths, and why the fast one is not enough

**The sweep** is the contract: every target, every interval, unconditionally. It
drives what no event announces — a grace period expiring, a retry becoming due —
and it is what catches up after the relay was down.

**The push** is an optimisation. `event-bus` bumps a sequence key per topic on
every publish, so a watch on `eb.seq.>` turns a poll interval into milliseconds.
`components/event-pusher` already does this from the other side on hosts with a
`wasmcloud:messaging` plugin.

Push does **not** replace the sweep, and this is the part most likely to be got
wrong later. Core-NATS delivery is at-most-once, so a relay that is restarting
drops every notification published while it was gone; and no KV change announces
the passage of time. `event-pusher`'s own header says the same about itself. A
system that kept only the push would work in every test and lose events in
production.

## One firing relay, for a sharper reason than the reconciler's

The reconciler takes a lease because two loops disagree about a scale-down
cooldown (ADR-0072). The relay's case is worse.

Two relays double-poking a **timer** is merely wasteful: `timer.due` leases what
it hands out, so the second caller gets nothing. But `bus.poll` **advances a
consumer group's offset**, and two relays draining one group race over which sees
an event — at-least-once silently becomes at-most-once. So the relay contends for
a JetStream KV lease of its own (`<lattice>-relay`, never the reconciler's key,
or the two would evict each other).

Without `--nats-url` it fires alone and says so on startup. That is correct for
tier 1, where one box runs one copy of one app and there is nothing to elect.

## An allow-list, not a discovery mechanism

Every target is named on the command line, there is no wildcard, and a relay
started with none refuses to start. A URL from configuration is a request this
process makes on somebody else's behalf, and a poker aimed at an arbitrary host is
a confused deputy with a timer attached. `comp-checks` takes `--allow` and
`comp-fswatch` takes `--allow-path` for the same reason.

## Consequences

- An app gains a trigger by declaring `[triggers]` in its spec, and gains nothing
  otherwise — no app acquires a process poking it every ten seconds by default.
- The relay dials `127.0.0.1:<port>`, never the app's public hostname, so an
  `/internal/*` endpoint does not become reachable wherever the route is.
- It JITs nothing, unlike `comp-host`, so its unit sets
  `MemoryDenyWriteExecute=yes` — the one hardening tier 1 cannot apply to the
  runtime.
- `system-cron` stays contract-only. A cron string is turned into a
  `sched:timer` recurring job, and `schedule-every` is keyed and idempotent
  precisely so calling it on every boot is safe.

## What was measured

A `saga-domain` trip whose hotel leg fails arms a retry timer:

| | after 6 s |
|---|---|
| no relay | `running` — `[pending, pending, pending]` |
| relay at 1 s | `compensated` — `[compensated, failed, pending]` |

The control matters as much as the result: without the relay nothing advances,
so the daemon is doing the work rather than a background thread inside the app.
