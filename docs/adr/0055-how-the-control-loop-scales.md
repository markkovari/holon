
# 0055 — How the control loop scales, and the tenancy bug that found

Status: accepted. `plan()` is now linear in apps and in nodes; a cross-tenant
miscount is fixed on the way.

> **Extended ([ADR-0056](0056-a-converged-app-keeps-its-placement.md)):** the
> `after` column below is the pass that RANKS. A converged app now keeps its
> placement instead, so the steady-state pass at 1000 nodes × 10 000 apps is
> 46 ms rather than 1 292 ms.

## The question

"How does this hold up with a lot of nodes and a lot of orgs?" Every number in
this repo so far is one or two nodes and a handful of apps. `plan()` is a pure
function, so unlike everything else the answer needs no fleet, no NATS and no
guessing — build a world in memory and time it. `comp-planscale` does that.

Worlds are `apps` apps over `apps/10` tenants, two replicas each, 50 distinct
artifacts shared between them, already converged — because the pass that
changes nothing is the pass the loop spends its life running.

## What it found

```
  nodes   apps  insts │  before   after  │ inv KiB/node  read MiB/pass
     10    100    200 │    0.38    0.71  │          3.6           0.04
     10   1000   2000 │   19.53    4.74  │         35.6           0.35
     10  10000  20000 │ 1763.51   33.74  │        360.8           3.52
    100  10000  20000 │ 1967.67  132.68  │         36.2           3.54
   1000    100    200 │    7.22   12.19  │          0.3           0.19
   1000  10000  20000 │ 2745.69 1291.63  │          3.8           3.67
```

Before, ten times the apps cost ninety times the pass: **quadratic**. Each
manifest scanned every node's whole instance list twice — once to total its
replicas, once per node to rank it — so a pass cost `apps × instances`, and
instances grow with apps.

Now both are answered from one index built once per pass: `(tenant, app,
component, digest, node) -> count`, plus a per-node total. Linear in each axis,
and 52× faster at ten nodes and ten thousand apps.

## The bug the index exposed

The old count matched on **component id and digest alone**. Two tenants running
the same catalogue component — both calling it `gate`, because that is what the
example calls it — counted each other's replicas as their own. Alice deploys,
bob deploys, and the loop sees alice's replica as satisfying bob's manifest.

That is precisely the case this platform exists for: one popular component,
many orgs. It was invisible because every test and every benchmark to date ran
one tenant per component id.

The index key includes the owner, so the fix came with the optimisation rather
than after it. `running_on(component, node)` is deleted rather than repaired —
a helper that *can* be called without a tenant is a helper that will be.

The same blindness was in the test harness's fake host, which merged two
tenants' instances into one row, so the model the tests asserted against was
wrong in the same direction as the code. Both fixed; the new test
(`one_tenants_replicas_do_not_count_as_anothers`) fails on either alone.

## Two cheaper things, while here

**Borrowed keys.** The first version of the index allocated five `String`s per
node per app to look up. At 1000 nodes it cost more than the scan it replaced —
the 1000-node column got *slower*. The nested `owner -> node -> count` map with
`&str` keys is one lookup per app and a borrow per node.

**Partial sort.** Spread only reads the first `replicas` entries of the ranking,
so `select_nth_unstable_by` partitions there and sorts the prefix. Identical
result, O(nodes) instead of O(nodes log nodes) per app. Worth 2× at a thousand
nodes and nothing at ten, which is the right shape for a change that costs six
lines.

## Where the walls actually are

Ranked by which is hit first, with the number that says so:

1. **The inventory snapshot, at ~2000 instances on one node.** 360 KiB of JSON,
   published every heartbeat. NATS refuses a message over 1 MiB, so the ceiling
   is roughly **5 500 instances per node** — and `plan.rs` already carries a
   `ponytail:` note saying deltas replace snapshots at exactly that point. It is
   now a measured number rather than a hunch.
2. **The reconciler reads the whole world every pass** — 3.5 MiB from KV at
   10 000 apps, independent of node count. Fine at a five-second interval,
   wasteful forever.
3. **One `plan()` at 1000 nodes × 10 000 apps is 1.29 s.** Still under a pass
   interval, but it is `apps × nodes` and that product is the thing to watch.
   The fix is sharding the loop by tenant, which the substrate already allows
   (each shard subscribes to its own subjects); it is not built.
4. **One reconciler.** There is no leader election, so scaling out the loop is
   an unbuilt design and not a config change.

## Bounds

- Synthetic worlds: one component per app, `spread`, no constraints, no
  autoscaling, converged. Constraints would make `fits` the hot loop instead,
  and it is a linear scan of labels per node per app.
- Timing is the median of five on this Mac. The shape is the finding, not the
  absolute milliseconds.
- Nothing here measures the *hosts* at these counts — 10 000 apps across ten
  nodes is a thousand instances each, and ADR-0053/0054 measured thirty-two.
  The control loop keeping up says nothing about the data plane.
