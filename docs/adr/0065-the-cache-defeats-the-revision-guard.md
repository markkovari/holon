# 0065 — The cache defeats the revision guard

Status: accepted, as a finding — and **fixed** by
[ADR-0066](0066-the-guard-moves-into-the-store.md), which moved the comparison
into the store where it belongs. The defect below is real, was measured, and no
longer reproduces; the test that found it now asserts three appends survive.

## What was left to measure

[ADR-0064](0064-the-cross-node-cost-of-the-read-cache.md) measured a stale read
across nodes and found it bounded by the TTL, then named what it had not reached:
two nodes *writing* one key, each holding its own cached read. A lost-update
shape, which no TTL bounds — the item is simply gone.

## Two writers alternating lose nothing

Twelve appends to one batch, alternating between two nodes:

```
no cache             12 accepted, 12 survived
--kv-cache-ms 1000   12 accepted, 12 survived
```

The reason is the one that also saved the rate limiter in ADR-0064:
**self-invalidation**. A node that writes a key drops its own copy first, so its
next read is a miss and the compare-and-set compares against truth. A node that
writes regularly never serves itself a stale copy of what it writes.

## A writer that READS between its writes does lose one

That is the ordering alternating writes never produce. A read in between puts a
copy back that the node's own write had dropped:

```
node 2 appends            batch = [2a]              node 2 caches nothing (own write)
node 2 reads              batch = [2a]              node 2 now HOLDS a copy
node 1 appends            batch = [2a, 1]           node 2's copy is now stale
node 2 appends            node 2 claimed size 2     built on the stale copy

three appends accepted, the store holds TWO
```

Node 1's item is gone. Not stale — gone, permanently, and no TTL touches it.

## Why the revision guard did not stop it

`record-store::update` takes an `expected_revision` and enforces it like this:

```rust
let current = load_record(&bucket, &collection, &id)?...;
if expected_revision != 0 && expected_revision != current.revision {
    return Err(StoreError::RevisionConflict(current.revision));
}
…
let stored = Stored { data, revision: current.revision + 1, … };
put_record(&bucket, &collection, &id, &stored)?;
```

It is a **read-compare-write over `wasi:keyvalue`**, not a store-native
compare-and-set. Every term in the comparison comes from `load_record` — so
through a cache, the guard compares a stale expectation against a stale current,
agrees with itself, and computes the new revision from the stale one too. It
passes on state that no longer exists.

The guard is not weakened by the cache. It is *bypassed*, silently, and it cannot
detect that it was: nothing in that code can tell a cached read from a fresh one.

**This is not a batching bug.** `record-store`'s revisions are what `conduit`,
`platform-domain` and `gate-domain` all build their concurrency on. Any of them,
on any key written from two nodes with a read in between, has this.

## What would actually fix it

A store-native compare-and-set. NATS KV has revisions and a revision-guarded
update; the contract does not expose one, so `record-store` emulates it over
get/put and inherits whatever the get did. A `wasi:keyvalue` extension (or a
`comp:`-local interface) offering `set-if-revision` would let the store enforce
the guard where the data is, and a cached read could then only cause a *retry*,
never a clobber.

That is the honest fix, and it is a contract change, not a flag.

Two things that are **not** fixes:

- *Do not cache keys this node has written.* It closes the measured ordering and
  not the general one: a node that has never written a key can still read it
  stale and then write it for the first time.
- *A shorter TTL.* It narrows the window and cannot close it. A lost update at
  10 ms is a lost update.

## Where this leaves the flag

| the key is… | with `--kv-cache-ms` |
|---|---|
| read and written by the same node only | safe, by self-invalidation |
| written by one node, read by others | stale up to the TTL ([0064](0064-the-cross-node-cost-of-the-read-cache.md)) |
| written by more than one node | **can lose a write** |
| never written | safe |

Sound for a read-only replica set, a single-writer app, or a single node.
Unsound, silently, for anything else — which is most of what a platform runs.

The performance case in ADR-0063 is unchanged and remains large: 99.7% of reads
served, durable storage at in-memory speed. It is simply not collectable by
default until the guard is enforced where the data is.
