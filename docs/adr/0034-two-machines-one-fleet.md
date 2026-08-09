# 0034 — Two machines, one fleet: placement does not map tenants to computers

Status: accepted. Extends ADR-0033 (two orgs under load) across a second machine.

> **Correction (ADR-0036):** the throughput figures below (~102 000 rps per org)
> are wrong — they were the ingress answering 503 with no route, which never
> proxies. Corrected: ~3 300 rps per org, p99 17 ms, 100% `200`. The placement,
> isolation and memory results are unaffected.


## The claim being tested

ADR-0033 measured two organisations on one box. The obvious way to pass that test
on two boxes is to give each org a box, and it would look identical in every
metric — while quietly turning a multi-tenant platform into two single-tenant
ones with a shared login page. So the assertion here is not "it runs on two
machines". It is:

> **every node holds instances from more than one organisation.**

Fleet: 3 `comp-host` nodes on the Mac (M-series, 10 cores) and 2 on a Raspberry
Pi 5 (4 cores, aarch64), joined over the LAN through one NATS. Six apps — three
per org — `fused`, one replica each.

## What placement did, and the bug it exposed first

The first run put all six apps on the alphabetically-first node. `Mode::Spread`
ranked candidates by `running_on(c, n)` — replicas **of that component** — so six
*different* apps all scored 0 on every node and the tie-break fell through to the
node name. Reproduced as a unit test before it was fixed: six apps, three nodes,
`[6, 0, 0]`.

Two changes in `plan.rs`, both needed:

1. Rank on three keys: replicas of this component (descending, so an existing
   placement is stable), then the node's **total** instance count (ascending, which
   is the cross-app balance that was missing), then the name for determinism.
2. A `pending` tally threaded through one `plan()` pass. Without it every app in a
   pass ranks against the same unchanged inventory and they all pick the same
   node — the inventory only catches up a heartbeat later, by which time the whole
   batch has landed in one place.

Result, from the run:

```
n1    acme + globex   <- both orgs
n2    acme + globex   <- both orgs
n3    acme + globex   <- both orgs
pi-1  acme + globex   <- both orgs

4/4 node(s) hold BOTH organisations — tenants are NOT mapped to machines
```

`pi-2` drew nothing: six apps over five nodes leaves one node empty, and the
ranking is deterministic about which. That is the placement rule behaving, not a
node failing — it heartbeats throughout and idles at 12 MiB.

## The numbers from the same run

| | |
|---|---|
| acme, 15s × 30 conns | 102 139 rps, p50 0.27 ms, p99 0.73 ms, 100% success |
| globex, same window | 102 632 rps, p50 0.27 ms, p99 0.70 ms, 100% success |
| cross-org read after load | 404 |
| cross-org deploy after load | 404 |
| Mac node RSS, 2 apps each | 52 MiB |
| Pi node RSS, apps / idle | 41 MiB / 12 MiB |

Load is generated on the Mac against the Mac's ingress, so those rps are a
Mac-local figure; what the Pi contributes to this table is that it holds a
mixed-tenant workload at 41 MiB, not throughput. A cross-machine throughput
number would need the load generator off-box and is not claimed here.

## The failure that cost the most time

Two runs placed nothing on either Pi node. The Pi logs said only

```
comp-host: heartbeat failed: publishing inventory
```

which is the top of an `anyhow` chain printed with `{e}`. Printed with `{e:#}` it
reads `ack error: timed out: didn't receive ack in time` — a JetStream publish ack
that never came back. It reproduces only while the Mac is saturated by the load
phase: the same `nats kv ls` run from the shell returns nothing during that window
too, so the inventory bucket is not lost, the box is simply too busy to ack within
5s. A remote node feels it first because it has a round-trip to lose.

Worth stating plainly: **a missed heartbeat is not death** (ADR-0022) and this
proves the other half of that — the Pi kept serving its instances throughout,
because nothing about an unreachable bus tells a node to stop. The cost was
diagnostic, not availability, and the fix was one format specifier. Any error
printed as `{e}` is a chain with its cause thrown away.

## What is still not proven

- Load never crosses machines. Both generators run on the Mac.
- Nothing was killed. Failover across machines is ADR-0029's single-machine result,
  untested with a real network partition between nodes.
- Five nodes, six apps. Whether the ranking still balances at a hundred apps is
  arithmetic nobody has run.
