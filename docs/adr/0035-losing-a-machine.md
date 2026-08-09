# 0035 — Losing a machine, measured through the failure

Status: accepted. Completes ADR-0034 (two machines, one fleet), which placed work
across two boxes but never killed one.

## What was missing

`bench/adversarial/five-nodes.sh` already kills the Pi nodes — then sleeps 20s and
sends 60 requests. That asks "did it recover" and skips "what did it cost". The
window it sleeps through is the only interesting one: between a machine dying and
the ingress noticing, requests are being routed at a corpse.

So: constant load through one address while nodes are `SIGKILL`ed underneath it,
bucketed per second, in two directions that are not the same claim —

- kill a node on **this** machine; the replacement must land on the Pi
- kill the **whole Pi**; the replacements must come home

Only the first proves recovery is not machine-local. Only the second proves
surviving a machine. Fleet: 2 nodes here, 2 on the Pi, one app, `replicas: 5`.

## The result

| | |
|---|---|
| requests across both kills | 258 612 |
| failed | **0** (0.00%) |
| kill mac-2 → noticed | 11s (inventory TTL) |
| kill mac-2 → back to 5 replicas | 17s (6s to re-place) |
| kill the Pi → noticed | 12s |
| kill the Pi → back to 5 replicas | 16s (4s to re-place) |
| final placement | mac-1 holding 5/5 |

The gap between "noticed" and "killed" is ADR-0022's rule working: a missed
heartbeat is not death, so the entry lives out its TTL first. The re-place itself
costs 4–6s, which is the reconcile interval plus an artifact already in cache.

## The bug the first run found, and the one it nearly hid

The **first** run was not clean: killing mac-2 cost nothing, and killing the Pi cost
113 requests over a 13s window. The tempting reading — remote failures are worse
than local ones — is wrong, and the code says so plainly.

The ingress retried exactly once (`.take(2)`). Least-outstanding ranks by requests
in flight, and **a dead node has none**, so a corpse sorts to the *front* of the
ranking. One dead node is survivable: first choice refuses instantly, the single
retry finds a live replica. Two dead nodes in the top two exhausts the budget and
the request 502s. Killing a two-node machine when three nodes remain puts two
corpses at the top of a three-node ranking — which is exactly what happened, and it
has nothing to do with the network.

The fix distinguishes the two failures the old loop treated alike: walk the whole
ranking past refused connections (an instant RST costs nothing to skip), but budget
timeouts at two, since a slow backend is what turns a retry into a stampede. Same
scenario after the change: **0 failed out of 268 081**, then confirmed again on the
run above. `bench/adversarial/slow-backend.sh` still shows least-outstanding at
6 699 rps against round-robin's 553 with 100% success, so the timeout budget that
protects against a slow node is intact.

## Two measurement bugs, both of which produced confident wrong numbers

Worth recording because both were *plausible* readings, which is the failure mode
this project keeps hitting:

1. **The first report claimed a 59s recovery.** It attributed every error after the
   first kill to the first kill, including errors that belonged to the second one.
   Windows are now bounded by the next event.
2. **The convergence sampler read empty for exactly the interval being measured.**
   `nats kv get --raw` writes no trailing newline, so several nodes concatenate into
   one invalid JSON line — it only parsed once the fleet was down to a single node.
   The published trace would have been "recovered in 1s", derived entirely from
   readings taken after everything was over. A second bug in the same code latched
   onto the total *before* the dip, which reads as instant recovery for the same
   reason. Both are now visible in the trace line, which prints every sample.

## What is still not proven

- Load originates on the Mac. A partition that isolates the Pi *from the load* is a
  different test than killing the process on it.
- The Pi was killed with `pkill -9`, so its listener sends RST. A machine that
  vanishes (cable, power) black-holes instead, and black-holed connections hit the
  timeout budget rather than the refusal path — the number that matters there is the
  3s client timeout, unmeasured.
- Nothing rebalances when a machine comes *back*. mac-1 ended holding 5/5 and keeps
  them; rejoining capacity stays idle until something else changes.
