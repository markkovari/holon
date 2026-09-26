# ADR-0100 — a parked turn is a record, not a held thread

*Every outbound call an agent makes has a reply that may arrive a second later
or an hour later. Nothing here should have to guess which.*

**Status: accepted.** Names the shape of `comp-park` before any of it is built,
the way ADR-0099 named holon-vcs's shape before `crates/holon-vcs` existed.

## The problem

An agent turn dispatches an outbound call — to a model, a tool, another
agent's queue, a batch job that takes minutes — and then needs to do nothing
until the answer shows up. Two ways that goes wrong today:

- **A blocked thread per call.** The calling process sits in a blocking read
  waiting for a response. It survives the wait but not a crash or a restart,
  and N outstanding calls costs N threads' worth of stack and a process that
  cannot be redeployed without losing every one of them.
- **No record at all.** If the process that dispatched the call dies before
  the answer lands, nothing else in the fleet knows the call was ever made —
  not "pending", not "answered", nothing. The answer arrives at a process that
  no longer exists to read it.

What's missing is not compute. It's a durable record of *what was asked, that
it is still outstanding, and what to do once it isn't* — independent of
whether the process that asked is still running.

## What this is not

Two systems were read closely before writing this (their READMEs and
`internal/wakeupprobe`, `demos/parking`, `DESIGN.md`): [Agent
Substrate](https://github.com/agent-substrate/substrate) and
[`google/ax`](https://github.com/google/ax) built on top of it. Both solve a
real problem — Substrate multiplexes millions of *mostly-idle sandboxes* onto
a small worker pool by suspending a whole VM's RAM and filesystem to object
storage and restoring it elsewhere in under 500ms; `ax`'s request-parking demo
holds inbound HTTP at the router while the pool is momentarily full, retrying
resume until a worker frees up. Both are real infrastructure for a real
problem: **sandbox density** on a Kubernetes cluster.

That is not this repository's problem. Nothing here runs a fleet of idle
containers that need multiplexing onto scarce hardware — `host/` starts a
component in 0.43ms (ADR-0040) and throws it away; there is no "warm sandbox"
to keep alive between turns in the first place. Adopting Substrate's mechanism
to solve "remember that a call is pending" would mean adopting Kubernetes, a
custom gVisor/microVM dataplane, and snapshot storage to persist a *process
image* — for a problem that is actually about persisting a *record*. The
gap between "checkpoint 4GB of RAM" and "write one line to a log" is the
whole reason this is its own ADR instead of "go install Substrate."

The one idea worth keeping from `ax`'s parking demo: **hold and retry rather
than fail fast when the thing you need is momentarily busy.** Kept below, at a
much smaller scale — a bounded queue and a redelivery, not a router.

## The decision

A parked turn is one more instance of the oplog-over-JetStream shape this
repository has now built twice — holon-vcs's write-ahead-intent (ADR-0099) and
`comp-media`'s `MEDIA_JOBS` work-queue. `comp-park` is the third:

- **A durable record per outstanding call**, written *before* the call is
  dispatched (`parked`) and closed only after the answer has been both
  received (`answered`) and consumed by whatever resumes the turn (`resumed`).
  An entry stuck at `parked` past its own deadline is exactly what `verify` /
  `repair` is for in holon-vcs — a crash between "sent" and "heard back" must
  read as "still outstanding", never as "never happened" or "done".
- **A wake is a message, not a woken thread.** When an answer arrives — a
  webhook, a poller noticing a batch job finished, `comp-goalrun` noticing a
  branch's checks came back — it is published once to a JetStream work-queue
  stream (`PARK_WAKE`, the same `RetentionPolicy::WorkQueue` shape as
  `MEDIA_JOBS`). A small, fixed pool of resumer workers pulls from it — nobody
  is blocked waiting; the record is what's waiting.
- **Resuming is somebody else's job.** `comp-park` does not know what an
  agent does with an answer, the same way `comp-checks` does not know what a
  goal is (ADR-0081's own framing). It hands the resumer worker `{session,
  ticket, call-result}` and steps back; whatever drives the agent (a
  component, `comp-goalrun`) reads it and decides what happens next.

## What this does license, from `ax`'s one good idea

`park()` on an already-saturated resumer pool does not refuse. It queues,
bounded (`--max-parked`, mirroring `comp-checks --max-concurrent`'s reasoning:
a generous ceiling against a pathological spike, not a throttle the mechanism
needs) — the caller gets a ticket back immediately either way, so "parked
behind other work" and "dispatched now" look identical from outside. That is
the one thing worth carrying over from `atenet`'s router: absorb transient
saturation instead of surfacing it as a failure that says nothing about
whether the call itself was good.

## Per ADR-0095

Applying the three questions:

1. **Needs a capability WASI does not give a guest?** Yes — a held JetStream
   subscription, a webhook listener, and a background poller for calls that
   are checked rather than pushed. Same reason `reconciler/` is native.
2. **Is it the smallest thing that needs to be?** `comp-park` holds the
   subscription, the timer, and the oplog. It does not know what a session
   is *for* — that stays in whatever component or daemon drives the turn.
3. **Does it answer a contract a component could have answered?** Yes:
   `wit/park/park.wit`'s `holon:park/lot` interface, served over HTTP by
   `comp-park`, the same shape as `holon:vcs/code-store` and `comp-vcs`.

## Shape, concretely

```
       agent turn                          comp-park                    resumer
    (component or daemon)                (native, JetStream)         (small pool)
           │                                    │                         │
           │  park(session, call) ──────────────▶                        │
           │◀───────────────── ticket           │  write "parked"        │
           │                                    │  (intent, before       │
           │  [does nothing else with           │   dispatching)         │
           │   this turn until woken]            │                         │
           │                                    │  dispatch the call     │
           │                                    │  (or hand it to a      │
           │                                    │   poller/webhook)      │
           │                                    │                         │
           │                       answer lands  │                         │
           │                                    │  write "answered",     │
           │                                    │  publish to PARK_WAKE ─┼─▶ pulls, acks
           │                                    │                         │  loads session,
           │◀════════ resumed with the call result, by whatever reads PARK_WAKE ═══╝
           │  write "resumed"                   │                         │
```

## Consequences

- A parked call survives the crash of everything except JetStream itself —
  the same guarantee holon-vcs already gives a patch.
- Nothing here holds an OS thread per outstanding call; the ceiling on
  concurrent *parked* turns is JetStream storage, not stack memory.
- Built, in the order holon-vcs was: ADR + WIT + workspace, then the
  in-memory engine (tested), then the NATS adapter and `PARK_WAKE` (tested
  against a real JetStream), then `comp-park` served over HTTP, then
  `components/park-store` + `components/park-gateway` (`apps/park.toml`),
  proven end to end (`e2e/park.sh`) through the real component chain —
  test → comp-host (park-gateway ⊕ park-store) → comp-park → NATS JetStream.
- **Still not built:** a resumer worker pulling from `PARK_WAKE` and driving a
  parked turn back to life. That is deliberately somebody else's code —
  this ADR's contract does not know what a turn does with its answer, the
  same boundary `holon:vcs/code-store` draws around a patch.
