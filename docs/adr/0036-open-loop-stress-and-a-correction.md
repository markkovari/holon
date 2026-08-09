# 0036 — Open-loop stress from a third machine, and a correction to ADR-0033/0034

Status: accepted. **Corrects the throughput figures in ADR-0033 and ADR-0034.**

## The correction first

ADR-0033 and ADR-0034 both report roughly **102 000 rps per organisation at 100%
success**. That number is wrong, and it was wrong in the way that is hardest to
catch: every digit of it was really measured.

`oha`'s "success rate" counts **completed requests, not 2xx**. Reading the status
codes out of the saved run shows what those requests were:

```
Status code distribution:
  [503] 1532563 responses
```

Every one was the ingress answering `no replica of "shop.acme.bench.test" is
currently placed`. The apps had been renamed `shop1..shopN` when the benchmark grew
to several apps per org; the load generator still asked for `shop.<org>`. With no
route, the ingress never proxies — so the figure being celebrated was the cost of
looking up a missing key in a hash map, which is indeed very fast.

Re-run with the Host header fixed, the same harness on the same hardware:

| | ADR-0033/34 claimed | actually |
|---|---|---|
| per org, concurrent | 102 139 rps | **3 313 rps** |
| p50 | 0.27 ms | 8.6 ms |
| p99 | 0.73 ms | 17.1 ms |
| status codes | "100% success" | 100% `200` |

Everything else in those ADRs stands: placement, interleaving across orgs and
machines, the isolation checks, and the RSS figures were all read from inventory and
host logs, not from the load generator.

A second, smaller version of the same mistake was in the load body. `gate-domain` is
a rate limiter, and every request used the same key, so once routing was fixed the
run answered `429` to 44 567 of 48 112 requests — the reject path, which touches
none of the storage the real path does. With `capacity`/`refill` raised it is 100%
`200` at the same throughput. **Throughput barely moved between the reject path and
the work path**, which is itself the finding: the bottleneck is the request path,
not the component.

The harness now prints the status-code distribution on every run and says
`NO 2xx AT ALL — this measured an error path` when there are none. That check, not
the discipline, is what stops this recurring.

## What the stress test adds

ADR-0035 killed machines under load and lost zero requests. Two things flattered it:
the generator was **closed-loop** (fixed threads waiting for replies, so offered load
falls by itself when nodes die — the survivors never got the dead nodes' share) and
it ran **on the box under test**.

So: `oha -q` fixing an arrival *rate* with `--latency-correction`, driven from
**bobocat** (8-core Apple silicon, ~24 ms LAN RTT), against nodes here and on malna.

| phase | offered | served | p50 | p99 | errors |
|---|---|---|---|---|---|
| 1. ceiling (closed loop, 200 conns) | — | 4 485 rps | 23.5 ms | 631 ms | 5× 503 of 44 751 |
| 2. steady open loop | 2 690 rps | 2 688 | 16.9 ms | 388 ms | 9× 503 of 53 759 |
| 3. bursts, healthy fleet | 2 690 rps in 1 345-request spikes | 1 286 | 188 ms | 1 035 ms | none |
| 4. both local nodes killed, malna alone | 2 690 rps | **880** | 18.8 ms | **46 125 ms** | **none** |

**The ceiling measures the path, not the fleet.** 4 485 rps from another machine
against ~3 300 rps measured on-box is not a contradiction: different bottlenecks
(there, one org's share through loopback; here, 200 connections at 24 ms RTT). No
number generated over this network describes the platform's capacity.

**Bursts cost latency, not availability.** The same average rate delivered as
one-second spikes moves p50 from 17 ms to 188 ms and p99 from 388 ms to 1.0 s, with
zero errors. The fleet absorbs the spike by queueing it.

**Phase 4 is the result worth having.** With both fast nodes killed, malna is handed
the full arrival rate *and* the replicas it was not running. It serves 880 of
2 690 rps — and returns **zero errors, zero non-2xx**. The excess did not fail; it
**queued for 46 seconds**.

That is the honest reading of ADR-0035's "0 failed": true, and incomplete. There is
**no load shedding anywhere in this platform**. An overloaded node accepts every
connection and makes callers wait, without bound — which to a caller is worse than a
503, because a 503 can be retried elsewhere in milliseconds while a 46-second wait
holds a connection, a thread, and a user. The ingress has a per-request timeout; the
queue in front of it does not.

`// ponytail:` the cheap fix is a bounded in-flight count per backend at the ingress,
shedding with 503 past it — the `Busy` counter that already exists for
least-outstanding is the number to bound. Not built here: this ADR is the
measurement that justifies it, and a shedding threshold guessed without one is how
you end up shedding at 60% utilisation.

## What is still not proven

- Recovery *completed* under this overload (5/5 replicas on malna afterwards), but
  nothing measured how long the re-placement itself took while the node was
  saturated — the reconciler and the load compete for the same Pi.
- The 46-second p99 is bounded by the run, not by the system. A longer run would
  show a longer number; the queue was still growing.
- One load box. Whether bobocat itself was the limit in phase 1 is unmeasured.
