# 0072 — One loop at a time

Status: accepted, and built. Closes `CURRENT.md`'s "the loop does not shard" —
by not sharding.

## The item said sharding. The measurements say otherwise

The open item read: *"One reconciler, no leader election. A steady pass at 1000
nodes × 10 000 apps is 46 ms, but the pass after any fleet change is 1.23 s and
that one is `apps × nodes`."*

Two different complaints in one sentence, and only one of them is urgent. Against
a 10-second interval, 1.23 s is 12% of a tick — on a fleet a hundred times larger
than anything this has run. Sharding buys throughput nobody is short of, and it
costs a membership protocol, a partitioning scheme, and rebalancing when the set
of reconcilers changes.

What was actually missing is that **the reconciler was the only control component
with no standby**. The ingress has had one since ADR-0029 and `tests/ha.rs`
asserts it. If this process died, the fleet kept serving whatever it already ran
and silently stopped adapting: no scale-up, no re-placement after a node loss, no
distribution of new artifacts. Nothing alerts, because everything that is already
running keeps running.

So: leader election, and sharding stays unbuilt until a number asks for it.

## Running two was not a workaround

Start commands are absolute counts and idempotent (ADR-0022), so two loops
issuing the same starts would only be wasteful. **Scale-down is different.** It
waits for a surplus to persist across `settle_passes` consecutive passes, and
that counter lives in each process's `Hysteresis` — in memory, per process. Two
loops count separately, so they disagree about when the cooldown has elapsed and
both then issue stops. The distribution pass would double-push as well.

## A bucket whose expiry IS the lease

`comp-lease`, a JetStream KV bucket whose `max_age` is the lease duration:

- **acquire** — `create`, which lands only when the key is absent
- **renew** — `update` guarded by the revision we hold, which lands only if we
  are still the holder nobody replaced

A leader that stops renewing has its key expire on its own. There is no
lease-breaking protocol to get wrong and nothing to clean up after a process that
died badly, which is the failure that matters — a clean shutdown releases the key
and fails over immediately.

Losing the race and losing the lease are the same code path: stop leading, keep
asking.

Two deliberate choices:

- **A standby does nothing at all** — no distribution, no diff, no commands. It
  holds no inventory cache and no hysteresis, so on promotion its scale-down
  cooldown starts from zero. That is the safe direction: under-replication fires
  on the first pass that sees it, only removal waits.
- **An unreachable lease means "not leader"**, not an error. The loop's oldest
  rule is that not knowing means changing nothing — a failed inventory poll is not
  an empty fleet. A reconciler that cannot see the lease cannot see the inventory
  either.

If the lease bucket cannot be created at all, the reconciler says so loudly and
runs anyway. Refusing to start would turn leader election into a new way to lose
the entire control plane.

## Measured

`reconciler/tests/leader.rs` — two reconcilers on one lattice, 6-second lease
(30 s in production):

```
first process        "this process is now the leader (picur-31234)"
second process       "standing by — picur-31234 holds the lease"
                     …and issues no commands while standing by
kill the leader
second process       "this process is now the leader (picur-31240)"
                     …and the fleet still serves
```

Failover takes up to the lease TTL plus one interval.

## Two bugs the test found, both worth keeping

**A standby that starts as a standby said nothing.** The transition was logged on
change, and starting as a follower is not a change from the initial value — so an
operator starting a second reconciler saw a banner and then silence,
indistinguishable from a hung process. The state is `Option<bool>` now, so the
first pass always speaks.

**The test killed both reconcilers.** It matched on the command line, and a
standby is started with the same arguments by design. That made a working
takeover look broken. It kills by pid now.

## Still not built, on purpose

Sharding. When a pass after a fleet change stops fitting comfortably inside an
interval — the number to watch is `comp-planscale`'s cold column against
`--interval` — the shape is already implied: partition by app, since `plan()` is
keyed on `(tenant, app, component)` and apps do not interact. Until then it is a
protocol with no problem.
