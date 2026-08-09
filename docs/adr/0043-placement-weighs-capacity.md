# 0043 — Placement weighs capacity

Status: accepted. Takes the one thing ADR-0039 found wasmCloud doing better.

## What was wrong

ADR-0034 ranked nodes by instance count, so **a 4-core Pi and a 10-core laptop were
interchangeable**. Four replicas over those two machines split 2/2, and the Pi — which
ADR-0039 measured at 549 rps against the Mac's 6 199 — became the bottleneck for the
whole app while the big machine idled.

wasmCloud, measured in passing on the same two machines, placed **3 on the Mac and 1
on the Pi**, tracking the 10:4 core ratio. The host has published `capacity.cpus` all
along; the reconciler's `NodeInventory` simply had no field for it, so nothing read
it.

## The change

Two places, because ranking and splitting answer different questions.

**Ranking is by load per CPU**, cross-multiplied rather than divided so it stays
integer arithmetic and cannot divide by zero:

```rust
b.1.cmp(&a.1).then((a.2 * bw).cmp(&(b.2 * aw))).then(a.0.node.cmp(&b.0.node))
```

Ten instances on ten cores is busier than two on four, and the ranking now says so.
Without it the big machine keeps winning simply for being big.

**Splitting is proportional**, by largest remainder — integers again, because a plan
whose output depends on the order of a float summation is not a pure function. Every
split sums to exactly the total, asserted across a range of totals and weight sets: a
split that hands out five replicas when four were asked for is a bug that shows up as
cost.

## The case the tests caught

With **fewer replicas than nodes**, proportional-by-capacity and the ranking disagree
— and proportional is wrong. One replica over a busy 10-core and an idle 4-core goes
to the 10-core box on capacity alone, even though it is at 1.0 instances/core against
the Pi's 0.5.

So: fewer replicas than nodes takes the *ranking's* answer, one each. Capacity is for
dividing many replicas; the ranking already knows who should get the next one, because
it blends current load with size. Both branches have a test, and the second exists
only because writing the test surfaced the conflict.

| | before | after |
|---|---|---|
| 4 replicas, 10-core + 4-core | 2 / 2 | **3 / 1** |
| 1 replica, busy 10-core vs idle 4-core | 10-core | **4-core** |

## What is not addressed

- **Cores are a poor proxy for capacity.** A core on an M-series Mac is not a core on
  a Pi 5 — ADR-0039 measured 11× the throughput per machine, not 2.5×. Weighting by
  advertised cores gets the direction right and the magnitude wrong.
  `// ponytail:` cores are what a node can report without being benchmarked; weight by
  measured throughput when there is a number worth trusting.
- **Memory and pool slots are not weighed at all**, though the pooling budget is the
  thing that actually starves (ADR-0008).
- **A node advertising nothing counts as one core** — the pessimistic reading, so an
  older host still gets placed rather than disappearing or dividing by zero. Tested.
