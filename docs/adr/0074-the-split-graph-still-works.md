# 0074 — The split graph still works

Status: accepted, as a test. Restores verification of
[ADR-0032](0032-cross-node-invocation-and-what-the-hop-costs.md), which has had
none since the script that proved it was deleted.

## Why this and not something new

ADR-0032 measured one app's graph spanning two nodes and put the hop at ~4%. That
is a load-bearing claim: it is what makes "decompose an app into components"
affordable rather than a distributed-systems tax, and `CURRENT.md` leans on it.

The proof was `bench/adversarial/split-graph.sh`. It was deleted when other
scenarios became Rust tests, and nothing replaced it —
[ADR-0068's](0068-the-index-was-the-lossy-part.md) commit noted the gap and moved
on. `fixtures/split-graph.yaml` has been sitting in the tree since as the input to
a test that did not exist.

An unverified claim in this repo has a track record. The secret reader was
"accepted, and built" and had never been linked (ADR-0061). `list-keys` returned
corrupted names to nine components, with a test asserting the opposite that passed
because its examples dodged the ambiguity (ADR-0068). The RealWorld conformance
runner — this project's only external validation — pointed at a binary that had
not existed for months (ADR-0062). So the question was not "should this be
re-tested" but "how many more of these are there".

**The answer here is: it works.** Which is worth knowing precisely because the
last three times it was not.

## What makes it a real cross-node test

Placement has to be forced apart, or the planner is free to put all three
components on one node and every call is in-process. The fixture constrains `gate`
to `role=web` and `record-store` to `role=data`; the harness gained per-node
labels, because it had none and a constrained app was simply unschedulable.

Three assertions, and the first two are what make the third mean anything:

1. the components are on different nodes — and `record-store` is **not** also on
   the web node, or `gate` could be calling a local copy;
2. `gate` reports `links 2 interface(s) over wrpc` — **both** imports resolved
   remotely. Checking only for "over wrpc" would pass on a run where one of the two
   was satisfied locally, which is half an in-process call;
3. a real request through the ingress succeeds, which it cannot do without both
   remote calls working — `gate` cannot rate-limit without `shaper` or persist
   without `record-store`.

```
comp-host: alice/split/gate links 2 interface(s) over wrpc
comp-host: alice/split/gate bound 2 interface(s) over wrpc
comp-host: started alice/split/gate … in 57 329 us
comp-host: started alice/split/shaper … in 9 619 us
comp-host: started alice/split/record-store … in 47 861 us
```

The test prints the lines it matched. A test that says only "passed" is a test
nobody can audit later, and this whole ADR exists because of assertions nobody
audited.

## And the price, re-measured

`comp-hopcost` runs the same graph twice — pinned to one node, then split — and
loads both identically:

```
co-located   19 938 rps    1 003 us per request
split        18 863 rps    1 060 us per request

one cross-node call adds 57 us, which is 5.4% of THIS request
```

**ADR-0032's ~4% survives**, with a correction to what the number means: the
microseconds are the portable figure and the percentage is not. A hop is a share
of whatever else the request does, and this one deliberately does as little as
possible — in-memory store, one call, no work. Put a JetStream round trip in the
path and the same 57 µs is a much smaller fraction. That is how 4% and 5.4% are
both right, and it is ADR-0057's point about a latency column that was arithmetic,
arriving again.

## Three ways the measurement was wrong first

Worth recording, because each produced a confident number:

**17.1%** — the load helper drives one key, and `gate`'s rate limit is a
compare-and-set retry loop where every retry calls `shaper` again. On a contended
key the split arm pays several hops per request, so this priced contention
amplification. Fixed with a distinct key per request.

**−0.1%** — a fresh HTTP client per request, so every request opened a TCP
connection. That cost both arms the same and buried the hop under it. Throughput
went from 1 951 to 18 490 rps when the client was reused, and only then was there
anything to see.

**Still −0.1%** — and this is the instructive one. `shaper` was **unconstrained**
in `split-graph.yaml`, so the planner was free to put it beside `gate`. Since
ADR-0070, `/api/ratelimit` calls only `shaper`. The "split" arm had no hop in it
at all, while the file's own comment claimed "every call crosses the bus". The
fixture now pins it, and the benchmark **verifies the placement of each arm before
trusting its number** — a co-located arm has to find shaper local, a split arm has
to find it remote, or it refuses to report.

A benchmark that cannot fail is a benchmark that measures nothing.
