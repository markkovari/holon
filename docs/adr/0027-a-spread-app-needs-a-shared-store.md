# ADR-0027 — A spread app needs a shared store, and the platform now refuses otherwise

- **Status:** accepted
- **Date:** 2026-08-08
- **Corrects:** [ADR-0023](0023-isolation-is-a-linker-boundary.md)'s backend table, which removed the NATS backend on a bad argument
- **Confirms:** [ADR-0026](0026-the-adversarial-run.md) still holds on the restored backend

## The bug

ADR-0025 celebrated a failover: the Pi died, its replica moved to the Mac, and the app
kept serving. Placement moved. **Data did not.**

Reproduced deliberately, one deployment with `replicas: 2` across two nodes:

```
node1 -> remaining 4.0
node1 -> remaining 3.001
node1 -> remaining 2.001
node2 -> remaining 4.0        <-- should have continued at ~1.0

stores on disk:
  n1: b-app-eve-shop  5 rows
  n2: b-app-eve-shop  5 rows   <-- same bucket name, two different files
```

The host names the bucket correctly and identically on both nodes — that part works, and
is what ADR-0023 was about. But `sqlite` and `memory` are **node-local**, so each replica
gets its own store under that shared name. A rate limiter stops rate-limiting. A session
vanishes on every other request. A counter counts wrong. Nothing errors.

This is worse than a crash, because a crash is visible.

## Why the wrong backend was the default

Honestly: a dependency conflict, dressed up afterwards as a security argument.

The synchronous `nats` 0.25 client pulls `nuid`, whose loose `rand` requirement cannot
unify with the `rand` `async-nats` needs — and `async-nats` is not optional, because the
lattice agent needs a held subscription. Rather than port the backend, ADR-0023 removed it
and justified the removal with: *a JetStream bucket is only a real boundary with per-tenant
accounts.*

That sentence is true and it is beside the point. Since the ADR-0012 fix the **host** names
the bucket, so guest-reachability is closed on every backend. Per-account isolation defends
against a compromised credential *holder*, not against a guest. A security-flavoured
argument was used to justify a decision a lockfile had already made, and the cost landed
somewhere the argument never looked.

## Decision

**Three things, and the third is the one that matters.**

1. **`NatsKv` is back, on `async-nats`.** `KvBackend` is a sync trait called from sync
   bindgen imports, so each method bridges with `block_in_place` + `Handle::block_on` —
   legal on the multi-threaded runtime, and it tells tokio the worker is about to block so
   the others are not starved. *(ponytail: the principled fix is async bindgen imports and
   an async `KvBackend`, a refactor touching every impl in `main.rs`. Do it when something
   other than this needs it.)*

2. **`increment` is a real compare-and-swap.** JetStream gives every entry a revision and
   `update` fails if it moved, so the backend retries rather than doing the
   read-modify-write the old synchronous client did. This makes NATS the only backend here
   with an increment that is atomic **across nodes** — which is precisely what a spread
   rate limiter needs. `wasi:keyvalue` still exposes no CAS to the guest, so a guest doing
   read-then-write across two calls is racy whatever this does (ADR-0008).

3. **A node advertises `kv_shared`, and the reconciler refuses to spread a stateful app
   onto nodes that answer false.** Derived from `host_needs` rather than a new `stateful:`
   flag, because `host_needs` is already stamped from the real WIT surface and a second
   source of truth could disagree with the imports. The refusal names the nodes and the fix:

   > `eve/shop unschedulable: spread across 2 nodes but ["n1", "n2"] have node-local stores`
   > `— every replica would get its own store under the same bucket name and diverge in`
   > `silence. Use replicas: 1, or run those nodes with a shared backend (--kv nats).`

   `kv_shared` defaults to **false** when absent. A node predating the field, or one whose
   inventory only partly parsed, must read as node-local; guessing "shared" would place an
   app somewhere it silently diverges, which is the failure this ADR exists about.

4. **Sharedness is a property a backend declares, not a name anything matches on.**
   `KvBackend::shared()` has no default implementation, so a new backend cannot forget to
   answer — and both possible guesses are wrong in expensive ways, `true` placing an app
   where it silently diverges and `false` refusing one that would have been fine. Nothing
   above `kv.rs` knows the word "nats": the host asks the backend it built, and the
   reconciler only ever sees a boolean in a node's inventory.

5. **A lattice node defaults to a shared backend.** The old default was `memory`, which on a
   lattice node is the worst of both: node-local *and* wiped on restart. The refusal in (3)
   only catches the spread case, so a single-replica app on `memory` lost everything on a
   restart with nothing said. A single-app run still defaults to `memory`, which is right
   for a lane that has no cluster at all. Choosing a node-local backend on a lattice node
   explicitly is still allowed, and warns.

   **The requirement is "shared", not "NATS".** An earlier draft of this ADR justified the
   default with *"NATS is already mandatory on a lattice, so defaulting to it costs
   nothing"* — which is the reasoning that turns a coincidence into coupling. The lattice
   uses NATS as a **transport**; the store needs to be **shared**. Those are two different
   requirements that one technology happens to satisfy, and writing them down as one is how
   a system ends up unable to change either. `kv::DEFAULT_SHARED` names the current pick in
   one place, and it is a default of availability — the only shared implementation that
   ships today — not an architectural requirement.

This is ADR-0013's "deny by omission" applied to storage: a capability nobody can partition
correctly is not granted at all.

## What it costs, measured on the same harness as ADR-0026

| | sqlite (node-local) | nats (shared) |
|---|---|---|
| requests/sec | 10,477 | 6,307 |
| p50 | 4.64 ms | 7.59 ms |
| p99 | 9.26 ms | 15.74 ms |
| two replicas share one store | **no** | **yes** |
| cross-node atomic increment | no | yes |

**~40% of throughput and ~1.6× the latency**, because every `get`/`set` becomes a round
trip instead of a local file write. That is the price of correctness for a spread stateful
app, and it is a real reason to keep sqlite rather than delete it — the single-node
self-hosting lane is where sqlite came from and where it remains exactly right, since
`replicas: 1` is the only option there anyway.

So the backend is a **deployment choice with a consequence the platform now enforces**,
not a default anyone should be picking by accident.

## ADR-0026 still holds

The adversarial sweep was re-run on the NATS backend: **0 foreign store opens, 0 keys read,
0 lateral connections**, same 16-name dictionary, same egress targets, same result. The
isolation claim is a property of the host's linker, not of the backend, which is what
ADR-0023 argued and this confirms by changing the backend underneath it.

## The transport is an interface too, in `comp/lattice`

Named first as a seam and then built, because the same argument applies to it: the fabric
between a node and the reconciler is not one dependency either. It is **three**, and they
want different things.

| trait | what it is for | wants |
|---|---|---|
| `Inventory` | nodes say what they run; entries expire | low latency, a TTL |
| `CommandBus` | start/stop, request/reply | low latency, an ack |
| `Artifacts` | bulk bytes by digest | durability, cheap storage |

`NatsLattice` implements all three, and the host and reconciler hold `Arc<dyn …>` — neither
binary names a broker any more. `async_nats` survives in exactly one place in the host,
`kv.rs`, which is the **store** and a different concern; that separation is the whole point
of this ADR and it is now visible in the imports.

Three traits rather than one because a second implementation of any single one is
plausible on its own: `--oci-mirror` in the reconciler is already most of a second
`Artifacts`, and swapping it should not require reimplementing command delivery.

**`MemoryLattice` implements all three as well, and that is not a toy.** An interface with
exactly one implementation has never been shown to be an interface: everything
broker-shaped that leaked into a signature shows up there as something awkward or
impossible to write. It found one real thing immediately — NATS applies `max_age` per
*bucket*, so `Inventory::publish`'s per-entry `ttl` is honoured at connect time rather than
per call. That is written down in `NatsLattice::connect` rather than left as a surprise.
It also lets the failover path be tested without waiting out a real TTL, and without a
broker running at all.

## What is still wrong

- **A single-replica app on a node-local store still loses its data when its node dies.**
  The reconciler will reschedule it onto a healthy node, where it will find an empty store.
  The default now makes this hard to reach by accident and the host warns when it is chosen
  deliberately, but nothing *prevents* it, because with `replicas: 1` there is no divergence
  to detect — only loss. Durability across a node failure is a different problem and is
  unaddressed; NATS with JetStream replication is the answer, and nothing here configures
  or verifies that.
- **`redis` reports `kv_shared: true`** and is therefore now spreadable, while ADR-0023
  correctly notes it is a naming convention rather than a boundary without ACLs. Shared and
  isolated are different properties and this field only claims the first.
- **No measurement on more than two nodes**, and none on the Pi.
