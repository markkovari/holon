# 0041 — The ingress sheds load

Status: accepted. Acts on the measurement in ADR-0036.

## What was wrong

ADR-0036 drove a fixed arrival rate at a fleet whose fast half had been killed. The
survivor served 880 of 2 690 rps with **zero errors and a p99 of 46 seconds**.
Nothing failed. Everything waited.

That is the worst behaviour measured in this project, and it looks like the best one
on a dashboard: 100% success, no errors, no alerts. A caller cannot see a queue it is
sitting in, cannot retry out of it, and holds a connection — and a thread, and a user
— for the whole time. A 503 it could take elsewhere in milliseconds is strictly
better information.

## The change

One flag: `--max-inflight` (default 64), bounding requests in flight **per node**.
What saturates is the machine, not the app, and an app that keeps queueing onto a
node everyone else is also waiting on is the thing to stop.

Two rules, and the second is the one that matters:

1. If **every** replica of a host is at the bound, refuse with 503 immediately.
2. If **some** replica has room, skip the saturated ones and use it — a single hot
   node must never cost an app a 503 while its siblings are idle.

`saturated()` is a free function so rule 2 can be tested directly; a rule that exists
only inside an async proxy loop is a rule nobody checks. `--max-inflight 0` restores
the old unbounded behaviour, and that escape hatch is total — tested at 999 in flight.

## The measurement

Same fleet, same 2 600 rps arrival rate, same generator; the only difference is the
flag. The overload phase kills both local nodes so malna alone takes the whole rate.

| overload phase | shedding off | shedding on (64) |
|---|---|---|
| p99 | **42 325 ms** | **747 ms** |
| p50 | 1.5 ms | 0.4 ms |
| served 2xx | 70 725 | **72 535** |
| shed 503 | 0 | 109 335 |

**A 57× better tail, and slightly more useful work done.** That second number is the
one worth dwelling on: shedding did not trade throughput for latency. The requests
that used to wait 42 seconds were not being served — they were occupying the queue
that made everything else slow. Refusing them immediately cost nothing and returned
capacity.

The healthy lanes are unaffected, which is the other half of the claim: baseline
2 600 rps at p99 75 ms → 50 ms, bursts unchanged at ~1 245 rps. Shedding costs
nothing when there is room.

## Honest notes on the run

- **The generator ran on this Mac**, not on the load box, which was asleep.
  ADR-0036 argues for off-box generation and that still holds — but this is an A/B of
  one ingress setting against another on the same fleet at the same rate, so both
  sides are affected identically and the comparison survives.
- **64 is a starting point, not a calibrated number.** It is high enough not to trip
  on a healthy fleet and low enough to bound the queue. The right value depends on
  what a node can take, which nodes do not advertise —
  `// ponytail:` the same missing input as capacity-weighted placement (ADR-0039).
- **109 335 sheds is not a bug.** The app was offered roughly three times what it
  could serve; the 503s are the platform telling the truth about that. What would be
  a bug is shedding while capacity sits idle, which rule 2 and its test exist to
  prevent.

## What this does not do

- **No `Retry-After`, no jitter, no backpressure signal upstream.** A shedding
  ingress that everyone retries instantly is a thundering herd with extra steps. This
  refuses; it does not yet advise.
- **Nothing feeds shedding back into autoscaling.** ADR-0038 scales on observed
  concurrency, and a shed request is not observed concurrency — so a sustained shed
  storm does not currently ask for more replicas, which is exactly when it should.
