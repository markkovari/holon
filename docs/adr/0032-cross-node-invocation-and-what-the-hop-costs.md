# ADR-0032 — Cross-node invocation works, and the hop is nearly free

- **Status:** accepted
- **Date:** 2026-08-08
- **Completes:** [ADR-0028](0028-cross-node-calls-are-wrpc.md), which chose wRPC and wired only the caller
- **Settles:** the deferral in [ADR-0025](0025-slice-one-on-the-lattice.md)

## It works

One application, graph split across two machines and two architectures:

```
MacBook   gate         (role=web)   imports records:store/store, shaper:limit/limiter
          shaper                    exports shaper:limit/limiter
Pi 5      record-store (role=data)  exports records:store/store   <- the other machine

gate bound 2 interface(s) over wrpc
record-store serves 8 function(s) to the lattice
shaper serves 2 function(s) to the lattice

HTTP to gate on the Mac:
  remaining 4.000   801.6 ms   (cold: compile + connect)
  remaining 3.790    67.2 ms
  remaining 2.857    39.7 ms
  remaining 1.897    40.8 ms
```

The counter advancing is the proof: rate-limit records can only be written and read through
`record-store`, which is on the Pi. A component called a component on another machine, and
neither knew — the import was bound in the linker, so the guest sees a function call.

## What the hop actually costs

**An earlier version of this ADR said 50×. That was a bad measurement and the number was
wrong.** It is kept below as the third column, because the mistake is more instructive than
the correction.

Same graph, same load, three placements:

| | co-located | split, two local nodes | split, Mac ↔ Pi |
|---|---|---|---|
| throughput | 2,788 rps | **2,682 rps** | 55 rps |
| p50 | 1.39 ms | **1.43 ms** | 41.9 ms |
| p99 | 2.40 ms | **2.56 ms** | 613 ms |

**The wRPC hop costs about 4% of throughput and 0.04 ms of median latency.** Between
comparable nodes it is very nearly free, which is roughly what ADR-0019 implied when it
priced an in-process link at 1.2 ms saved per hop against wasmCloud's lattice.

The 55 rps column measures something else entirely. That node is a Raspberry Pi **whose own
store is on the other machine** — so every `records:store` call went Mac → Pi over the LAN,
and then Pi → Mac again for each NATS KV read and write. A double crossing, terminating in
the slowest hardware in the fleet, which ADR-0030 had already measured standalone at 78 rps.

Attributing that to the RPC layer was wrong. It measured [ADR-0030](0030-least-outstanding.md)'s
finding — that a node whose store is remote is dramatically slower — a second time, in a
place where the transport happened to be in frame.

## What that means for placement

Co-location stays the default, but for a smaller reason than "50×": it is simpler, it needs
no bus, and it keeps a graph's failure modes together. Spanning is now a real option rather
than a last resort — a GPU-pinned component or a jurisdiction-bound one costs a few percent,
not two orders of magnitude.

The thing that *is* expensive is unchanged and was already known: **putting a component far
from its store.** That is a storage decision, not an RPC one, and ADR-0027's shared-store
requirement is what forces the trade.

## How the wrong number happened

Worth writing down, because it is the fourth measurement in this line of work to read
convincingly and mean something else:

* two nodes both returning `4.0` looked like isolation working, and also meant state was
  per-node (ADR-0027);
* a co-located graph binding its links over the bus looked like a start-ordering bug, and
  was the absence of a feature two ADRs had described as existing;
* a token bucket refilling looked like data loss after a node died (it had not);
* three nodes balancing perfectly looked like a passing five-node test, with two nodes
  silently absent on a stale binary;
* and here, a split graph looked like an RPC cost when it was a storage cost.

The common shape: **the measurement had more than one variable in it, and the result was
attributed to the interesting one.** The fix each time was the same — isolate one variable
and re-run. Two local nodes was a five-minute test that should have come before the
cross-machine one, not after it.

## The design, in one line each

- **Serve side**: one subscription per exported function, on the instance's own subject
  prefix, in a **queue group named for the instance** — so N replicas of a component share
  invocations and a departing one needs no deregistration. Exports are read from the
  component's own type, never a manifest that could drift from it.
- **Call side**: every link becomes a wRPC client, keyed by interface so one store reaches
  many targets. Including links whose target runs in the same process — see below.
- **A plug is now a first-class instance.** `Instance.pre` became `Option<ProxyPre>`:
  a component with no `wasi:http/incoming-handler` used to be unable to start at all, and
  now starts, serves its exports, and simply never appears in the route table.
- **A served invocation gets the same `Store` an HTTP request gets** — same scope, memory
  cap, CPU slice and egress allow-list. `store_for` is one function precisely so there is
  no second construction path for one of those to be forgotten from, which is what
  ADR-0023 is about.
- **Placement**: a component with its own constraints is placed independently; one without
  rides along with the root.

## There is no local short-circuit, and there was never going to be one

An earlier draft described co-located links taking a "direct in-process path", and treated
the fact that they did not as a start-ordering bug to fix. Both were wrong, and trying to
fix the non-bug is what exposed it.

**Two separately started components have no in-process path between them.** This host
satisfies an import from a host capability or from wRPC, and from nothing else. Skipping a
co-located target leaves its import unbound and the instance refuses to start:

```
could not relink alice/split/gate to local alice/split/record-store:
  component imports instance `records:store/store@0.1.0`, but a matching
  implementation was not found in the linker
```

Components that *do* link in-process were fused by `wac` at build time — ADR-0005's other
strategy, and a different mechanism entirely. `linked` has always meant "the host wires
them", and the host's only wire is the bus.

So every link is a wRPC client, including a loopback one. **It costs about 0.3%:** 2,798 rps
co-located over the bus, against 2,788 measured before, i.e. inside noise. An in-process
short-circuit would be worth a third of a percent and would first require building
instance-to-instance linking that does not exist.

The relink machinery written to "fix" the ordering has been deleted. It could not have
worked, and the delete is the honest outcome.

## Still not carried: resources

`link_instance` is given empty resource maps. wRPC encodes a resource as opaque bytes whose
meaning is application-specific, and nothing here defines that meaning, so an interface
passing one must be refused at placement rather than handed a blob the far side cannot
read. **Nothing classifies interfaces yet**, so that refusal does not exist — a graph
spanning a resource-bearing interface will fail at first call rather than at deploy. That is
the next thing to build, and it is the last claim in ADR-0028's list still outstanding.
