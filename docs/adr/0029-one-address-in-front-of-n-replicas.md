# ADR-0029 — One address in front of N replicas, and its table comes from inventory

- **Status:** accepted
- **Date:** 2026-08-08
- **Completes:** [ADR-0025](0025-slice-one-on-the-lattice.md), which placed an app on two nodes and left every caller having to know both

## Context

Placement across nodes did nothing for a caller that knows one address. Every run so far
reached a replica by `curl`ing a node's IP directly, which made "two replicas" true and
useless at the same time. ADR-0028 named this as the smaller of the two remaining pieces,
and it is: routing is not invocation, and it does not need a wire format.

## Decision

**`comp-ingress` terminates HTTP and forwards it to a node, directly.**

Not over the bus. Every node is mutually reachable on the tailnet and already advertises
where it can be reached, so the request goes as HTTP and comes back as HTTP — no envelope
to define, no serialization to get wrong, no hop through a broker on the data path. NATS
queue groups are the alternative and would be right if nodes were *not* directly reachable,
or if the bus should do the balancing; neither holds here, and the extra hop would cost
latency for nothing.

**Its routing table is built from inventory, not from the control plane.** A node now
advertises, per instance, the `Host` header it answers to, plus its own reachable address —
which is not derivable anywhere else, because a node bound to `0.0.0.0` knows its port and
not its address. So the ingress needs no platform credential, no manifest access, and keeps
routing while the control plane is down. That is the same property the node ledger buys on
the other side, applied to the data plane.

Three deliberately small choices:

- **Round robin over a sorted list.** Sorted so the rotation is stable across refreshes —
  inventory returns nodes in whatever order the KV lists them, and an unsorted list would
  make the "round robin" a random walk. *(ponytail: not least-connections or latency-aware;
  those need per-backend state and a feedback loop, and nothing has shown they are needed.)*
- **One backend per node, not per replica.** A node holding two replicas is still one place
  to send a request, and counting it twice would skew the rotation toward the busiest node.
- **A failed inventory read leaves the previous table in place.** Emptying it would 503
  every request because the *control* plane blinked, which is exactly what reading
  inventory instead of asking the platform was meant to avoid.

Failure handling is inventory expiry plus **one** retry against a different replica: a node
that stops heartbeating leaves the table within a TTL, and a node that dies between
refreshes costs one request a retry. Retrying further would turn one sick backend into a
stampede.

Every response carries `x-comp-node`. It is the single most useful thing an ingress can
say, and the only way to see the balance from outside.

## The measurement

Five nodes, two machines, two architectures: three `comp-host` on a MacBook, two on a
Raspberry Pi 5, one deployment with `replicas: 5`, one `comp-ingress`. **200 requests, all
to one address:**

| node | requests |
|---|---|
| mac-1 | 40 |
| mac-2 | 40 |
| mac-3 | 40 |
| pi-1 | 40 |
| pi-2 | 40 |
| **failed** | **0** |

Then both Pi nodes were killed and 60 more requests sent: **20 / 20 / 20 across the
surviving Mac nodes, zero failures.** No configuration changed and nothing was told about
the failure — the Pi entries expired from inventory and the table rebuilt itself.

## What this is not

- **Not invocation.** A component still cannot call a component on another node; that is
  ADR-0028's wRPC work and remains unbuilt. This routes *requests* to replicas, which is a
  different and much smaller problem.
- **Not highly available itself.** One `comp-ingress` is one process. It holds no state
  beyond a cache of inventory, so running several behind DNS or anycast is the obvious
  answer, but nothing here does that or tests it.
- **Not TLS.** It speaks plain HTTP in both directions. Termination belongs in front of it,
  which is what the `holon node render` Caddy lane already produces.
- **Not measured under load or with a slow backend.** The even split is from 200 sequential
  requests against healthy nodes. A round robin's weakness is precisely a backend that is
  up but slow, and that case has not been provoked.
