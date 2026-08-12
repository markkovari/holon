# 0067 — One copy is not a backup

Status: accepted, and built. Two of the four gaps in `CURRENT.md`'s durability
story; the other two (index writes, a repair pass) are named at the end.

## What was true before this

Every bucket this platform has ever created was made like this:

```rust
create_key_value(kv::Config { bucket: name, history: 1, ..Default::default() })
```

`num_replicas` defaults to 0, which JetStream reads as **one**, on `File` storage.
So one server's disk held the only copy of every tenant's data, and `history: 1`
meant there was not even a previous version of a key to go back to.

There was also no backup. Grepping the repo for backup, snapshot or restore
turned up `wasi_snapshot_preview1` and an ADR about inventory sizes.

[ADR-0035](0035-losing-a-machine.md) measured this fleet losing a **host** —
zero failed requests, replicas back in 16 seconds. That result is real and it is
about compute. Nothing had ever measured losing the **store**, and at one replica
the answer would have been: everything, permanently.

## Backup first, because it is the floor

`just backup` and `just restore`, over `nats stream backup` — the vendor's own
snapshot protocol, which streams a stream's messages *and* its configuration.
Writing our own would be re-implementing a wire format for nothing.

Verified the only way a backup can be: by destroying the data.

```
just backup                    KV_b-app-acme-shop -> backups/<utc>/
nats stream rm KV_b-app-…       deleted the bucket
nats kv get … order-1           nats: error: bucket not found
just restore                    KV_b-app-acme-shop
nats kv get … order-1           {"total":4200}
nats kv get … order-2           {"total":99}
just restore                    SKIP — it already exists
```

The refusal to overwrite an existing bucket is deliberate. Restoring over live
data is how a backup becomes an outage; an operator who means it can delete the
stream first and say so.

`REPLICAS=` on a restore overrides the copy count, which makes restore the way to
re-replicate a bucket that was created before any of this.

## Then replication, without anyone having to remember

`--kv-replicas` defaults to **0, meaning "as many as this NATS can hold, up to
3"**. The host asks for three and falls back to one *only* when the server is not
clustered, saying so loudly when it does.

The alternative — defaulting to 1 and warning — was the first version of this ADR
and it was wrong. A default that is safe only if you read the warning is a default
that loses data, and "3 breaks single-node deployments" is an argument for
handling that case, not for making everyone opt in to durability.

An **explicit** number is taken literally and never falls back. Asking for three
copies and quietly getting one is how you believe you are safe when you are not,
so that case fails to start instead:

```
A. clustered, no flag                Replicas: 3, no warning
B. single unclustered server         falls back to 1, warns, keeps serving
C. list whose FIRST address is dead  connects via the others, Replicas: 3
D. explicit --kv-replicas 3, solo    refuses — 503, no bucket created
```

## And one flag can name the whole cluster

`--nats-url` and `--lattice-nats` take a comma-separated list. A client given a
single address does learn its peers from the INFO the server sends and fails over
to them — but only after it has connected to something. A process starting while
its one listed server is the one that is down cannot bootstrap at all, which is
exactly the moment it matters. Row C above is that case.

Everything in the workspace that dials NATS now goes through one `servers()`
helper, so the store, the control bus, the reconciler and the ingress all take a
list rather than three different opinions about it.

## Measured: losing the server that holds the data

Three `nats-server`s clustered on one box — a real R3 cluster, same quorum code
as three machines — with `comp-host --kv-replicas 3` serving `gate-domain`'s rate
limiter, whose state is a counter in that bucket.

```
Replicas: 3    Leader: n1    Replica: n2, current    Replica: n3, current

before the kill:  remaining 93, 92, 91
                  *** killed n1, the stream leader ***
after the kill:   remaining 90, 89, 88, 87, 86

Leader: n3    Replica: n1, outdated, OFFLINE, 73 operations behind
```

Zero errors, and the counter kept counting down rather than resetting: the state
survived the loss of the server that led it. At one replica that same kill is
every tenant's data, gone.

The host in that run was given **one** NATS URL and kept working after that
server died, because NATS clients learn the cluster from the server's own INFO.
Nice to have, and not a thing to depend on — which is why the URL is now a list.

A cluster of three processes on one machine proves the *code*, not the
*hardware*. It exercises the same quorum, replication and election paths; what it
cannot show is a disk dying, a power loss, or a network partition between rooms.
For that the three copies have to be on three machines — which this fleet has.

## Still open, and both are `comp:store/cas` work

- **Index maintenance is unguarded.** `record-store`'s `ids_insert` is a
  read-modify-write over the chunked id list. Because `list`, `count` and `query`
  page over `idx_{collection}`, an id lost there makes a record that still exists
  **invisible** — indistinguishable from data loss, and silent.
  [ADR-0066](0066-the-guard-moves-into-the-store.md) built the primitive that
  fixes this and pointed it only at the record.
- **Nothing repairs an inconsistency.** A record and its indexes are separate
  writes; a crash between them leaves them disagreeing, with no way to notice or
  fix it. The records are authoritative, so a rebuild is mechanical — it just does
  not exist.
