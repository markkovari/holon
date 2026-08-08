# ADR-0026 — The adversarial run: contained, at 10.5k rps, in 56 MiB

- **Status:** accepted
- **Date:** 2026-08-08
- **Discharges:** [ADR-0023](0023-isolation-is-a-linker-boundary.md)'s falsifying measurement
- **Answers:** [ADR-0012](0012-keyvalue-isolation-needs-a-cooperative-component.md), which measured the leak this closes
- **Restores:** [ADR-0008](0008-isolation-is-stamped-never-authored.md)'s release gate, which ADR-0023 put back to unmet

## Context

ADR-0023 collapsed every tenant on a node into one `comp-host` process and said, in as
many words, that this makes the host's capability implementations a trust boundary and
that the claim was unproven until something attacked it. It ended by insisting the
isolation number and the throughput number come from **one run**, because measuring them
separately is how you get a density claim taken on an idle box and a safety claim taken on
a quiet one.

This is that run.

## Setup

One `comp-host` process (pid confirmed single), two tenants placed on it by the reconciler
from two manifests:

- `eve/shop` — the gateway component, seeded with real data and then driven under load.
- `alice/probe` — `components/adversary`, a component written to misbehave.

One sqlite file backing both, which is the harshest arrangement available: no file
boundary, no process boundary, nothing between the two tenants but the linker.

The adversary runs **while eve is under load**, not before or after.

## What it tried

**Stores.** A 16-entry dictionary of the shapes a leak would actually take: the exact
string the host derives for the neighbour (`b-app-eve-shop`), the shapes a naive prefix
scheme would let through (`../b-app-eve-shop`, `default/../b-app-eve-shop`,
`b-app-eve-shop/`), the neighbour's tenant and app names alone and together, case and
whitespace variants of `default`, the empty string, `*`, and `%2e%2e`. Every handle it
obtained was **used** — `list_keys` on each — because an open that reaches nothing is not
the thing worth counting.

**Egress.** The NATS bus this node is joined to, the same bus by a name that resolves to
it, the host's own listener, the cloud metadata endpoint, two Tailscale CGNAT addresses,
two RFC1918 addresses, IPv6 loopback — plus one ordinary public host as a **control**.

The control is not padding. A host that refused everything would pass a lateral-movement
test while being useless, and the two are indistinguishable unless something legitimate is
expected to get through.

The sweep ran twice, because the egress policy has two independent checks and the first
run only exercised one:

- **Run A** — alice's allow-list empty, the default. Tests the *name* check.
- **Run B** — **every hostile target explicitly allow-listed by name**, so only the
  resolved-address backstop can refuse. This is the DNS-rebinding shape: an allow-listed
  name pointing somewhere it must not.

## Results

| | Run A (deny-all) | Run B (all hostile names allow-listed) |
|---|---|---|
| foreign store opens | **0** / 15 | **0** / 15 |
| neighbour keys read | **0** | **0** |
| lateral connections | **0** / 9 | **0** / 9 |
| egress refused at *name* | 10 | 0 |
| egress refused at *address* | — | 9 |
| control (public host) | refused (correctly — not allow-listed) | **connected** (correctly — allow-listed) |
| requests/sec | 10,099 | 10,477 |
| p50 | 4.80 ms | 4.64 ms |
| p99 | 9.75 ms | 9.26 ms |
| success rate | 100% | 100% |

`default` opened successfully in both runs and returned **0 keys** — alice's own store,
correctly empty. That distinction matters: a run where `default` failed would be a broken
host, not a secure one.

Eve's bucket held **11,502 rows** by the end of run B. The adversary read none of them.

**Resident memory of the one process holding both tenants: 56 MiB.** ADR-0020 measured a
single-app host pod settling at ~233 Mi under load. That comparison is indicative, not
like-for-like — different substrate, different app, different machine — but it is the
first number taken on the lattice and it points the same way ADR-0019 did.

Run B is the interesting one. Nine targets whose names the tenant was **explicitly
permitted** to dial were still refused, each with the host logging the address it resolved
to:

```
alice/probe/adversary denied egress to localhost:4222 — it resolves to ::1
alice/probe/adversary denied egress to 169.254.169.254:80 — it resolves to 169.254.169.254
```

That is the backstop doing the job the name check cannot.

## Verdict

**Contained.** ADR-0008's release gate — "two tenants on one hostgroup, A provably cannot
read B" — is met again, this time on a shared process rather than by an OS boundary.
ADR-0012's finding is answered: the same guest string that leaked under wasmCloud now
resolves to a store the platform named, or to nothing.

## What this does NOT show, and I would rather say it here than be asked

- **ADR-0023's two unmitigated risks are untouched.** Cross-tenant timing side channels in
  a shared `Engine` and code cache are not tested by this and were never going to be;
  per-tenant memory accounting is still unsolved, and 56 MiB for two tenants tells you
  nothing about which tenant owns what inside it. Both remain open exactly as written.
- **The dictionary is not 10⁶ attempts.** ADR-0023 asked for `0 / 10^6`. That framing was
  wrong and is superseded here: a million random strings test a hash function, whereas
  sixteen strings chosen to match the shapes a real leak takes test the actual policy. The
  claim is "every shape we can think of, including the exact one that leaked before", not
  "a large number of shapes".
- **One node.** Cross-node subject probing is in the adversary's remit but not in this run,
  because cross-node WIT calls are not built (ADR-0025). Nothing here says anything about a
  tenant reaching another node's instances over the bus.
- **`refused:address` and "nobody was listening" share one error channel** in `wasi:http`,
  so the component cannot tell them apart. The host's log is what separates them, and it
  was checked. The count that matters — connections established — is unaffected either way.
- **This is a Mac.** The Pi is a second architecture and the run has not been repeated
  there.

## The instrument stays

`components/adversary` is committed, not thrown away. It is the thing to re-run when the
linker gains a capability, when a backend changes, or when anyone proposes relaxing the
address deny-list — and it prints a verdict rather than a wall of JSON so that re-running
it is cheap enough to actually happen.
