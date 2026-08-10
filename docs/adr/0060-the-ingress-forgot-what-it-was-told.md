
# 0060 — The ingress forgot what it had just been told

Status: accepted. A live bug, found by chasing a benchmark caveat.

ADR-0057 published a Pi run with a footnote: the ingress cell shed 3 412
requests where the Mac's shed none, attributed to four cores and a fixed
`max_inflight`. That attribution was wrong, and the note was the only reason
anyone looked.

## What it actually was

The Mac sheds too — 12 851 requests in one run, 13 452 in another, none in the
two between. A count and a status code could not say why, so both were
instrumented: the refusal text, and when in the run it happened.

```
  non-2xx at apps=1: 503x13452
    first said: no replica of "app0.matrix.test" is currently placed
    between 0.5s and 1.4s into the run
```

Not the shedding path — the **activation** path. And confined to a 0.9-second
window at the start, for an app that was placed and healthy throughout. The
host logged no heartbeat trouble and the reconciler issued no stop commands.

`activate()` begins by checking the routing table for a backend someone else
has already brought up:

```rust
// Someone may have activated while we waited for the lock.
if let Some(b) = table.read().unwrap().routes.get(host)... { return Some(b.clone()) }
```

**Nothing ever wrote what that check was looking for.** The table was only ever
replaced wholesale by the refresh timer, so the check could not succeed until
the next refresh — up to three seconds later. Every request to a host missing
from the table queued on the activation lock, took its turn, and made its own
round trip to the reconciler. Under load that is thousands of requests
serialised behind an answer the first one already had.

The fix is the missing half of the existing design: publish the backend into the
table. Six lines, and the comment above becomes true.

```
 before │ 25 741 rps  p99.9 13.77 ms  12 803 refusals   (2 runs in 4)
  after │ 25 957 rps  p99.9  5.92 ms       0 refusals   (4 runs in 4)
```

The p99.9 halving is the same bug seen from the other side: a request that
queued behind an activation and then succeeded shows up as a tail, not as an
error.

Guarded by a test whose control plane answers **exactly once**, so the second
activation can only succeed from what the first published. It fails on the
unfixed code.

## And the per-core bound, which was a real fix for a different problem

`max_inflight` was one number for every node. The placement ranking has divided
load by core count since [ADR-0055](0055-how-the-control-loop-scales.md) — "a
four-core Pi and a ten-core laptop are not interchangeable" — while the door had
not caught up, and the code carried a note saying per-node capacity was blocked
on nodes advertising what they can take. They have advertised it all along.

So the bound is per core now. It did not cause the Pi's shedding and it does not
fix it; it is correct on its own terms and was found by looking.

## The harness bug underneath the other caveat

The same ADR noted that nobody had measured 200 apps all busy. The reason was
in the load generator: each worker pinned itself to one app for the whole run,
so `min(workers, apps)` apps ever saw traffic. The 200-app cell had twelve
workers — it measured one app's throughput with 199 spectators, and reported it
as a 200-app result.

Workers now walk the whole host list, one app per request, offset per worker so
they do not march in step.

```
    1 app,  all busy │ 25 957 rps  p50 1.79 ms  p99.9 5.92 ms
  200 apps, all busy │ 31 972 rps  p50 1.48 ms  p99.9 3.37 ms   53.1 MiB
```

Two hundred tenants serving concurrently is *faster* than one, because load
spreads over two hundred instance paths instead of contending on one — the same
effect ADR-0053 saw and could not explain at the time. Idle cost stays at
0.21 MiB per app.

## Bounds

- The Pi has not been re-measured with either fix: it went off the network
  mid-session. Its 3 412 refusals are almost certainly the same activation bug,
  and "almost certainly" is not a measurement.
- Two hundred apps all busy is still one component doing one thing. A real
  tenant mix differs in what each app does, not only in how many there are.
- The activation entry carries `cpus: 1` until the next refresh corrects it,
  so a freshly activated node is treated as small for up to one refresh
  interval. That is the safe direction and it is a guess, not a measurement.
