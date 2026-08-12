# 0066 — The guard moves into the store

Status: accepted, and built. Fixes the lost update
[ADR-0065](0065-the-cache-defeats-the-revision-guard.md) measured.

## The defect, restated in one line

`record-store::update` enforced `expected_revision` by reading a record, comparing,
and writing it back over three separate `wasi:keyvalue` calls. Anything that
changed the record in between — another node, or a read cache — made the guard
agree with itself about state that was already gone. Three appends accepted, two
survived.

A guard whose comparison happens somewhere other than where the data lives is not
a guard. It is a suggestion with good intentions.

## `comp:store/cas`

The smallest primitive that puts the comparison in the right place:

```wit
/// The value together with its revision. A host may never serve this from a cache.
get: func(b: borrow<bucket>, key: string) -> result<option<versioned>, error>;

/// Write only if the key is still at `expected`. 0 means "must not exist yet".
set: func(b: borrow<bucket>, key: string, value: list<u8>, expected: u64)
    -> result<outcome, error>;
```

Deliberately not part of `wasi:keyvalue` — that is somebody else's contract, and
this is a `comp:` extension until the upstream one grows a CAS. Both calls take
the same `bucket` resource `wasi:keyvalue` already hands out, so the ADR-0012
boundary is untouched: a guest still cannot name a store it was not given.

Every backend implements it natively, and there is **no default implementation**
— for the same reason `shared()` has none. A backend that quietly emulated this
with get-then-set would compile, pass its tests, and lose writes.

| backend | what makes it atomic |
|---|---|
| NATS | JetStream's own revision on `update` — **atomic across machines**, which is the case that matters |
| SQLite | one `IMMEDIATE` transaction |
| Redis | one Lua script; a `MULTI` pipeline cannot branch on what it read |
| memory | one mutex |

`record-store::update` now loops: read through `cas::get`, check the caller's
expectation, write through `cas::set`, and on a conflict re-read and try again —
the retry it previously could not do, because it never found out it had lost.

## The second half, which was not obvious

Moving the guard alone turned the lost update into a **failed request**. Better —
no silent corruption — and still wrong: the losing writer re-read through the
cache, offered the same stale revision, and was refused again until the TTL
lapsed.

The fix is that `cas::get` **refreshes** the cache. That read is authoritative,
uncached by contract, and already in hand; storing it replaces exactly the stale
entry that was causing the refusals. Without it a writer cannot converge; with it
the retry succeeds on the next pass.

Two lines, and the difference between "does not corrupt" and "works".

## Measured

```
                        appends accepted   surviving
before (ADR-0065)              3               2      <- a write was lost
guard in the store             3               2      <- refused, not lost
+ the guarded read refreshes   3               3      <- and node 2's landed
```

Everything else held: RealWorld conformance 13/13 (154 requests), 167 tests across
four crates, and the performance is untouched —

```
GET /api/articles     6 117 -> 17 855 rps      99.7% of reads served
create                  352 ->  1 167 rps
```

— which is the same result as before the fix, so the guard costs nothing on the
read path it protects.

## Where the flag stands now

Still **off by default**, but the reason has changed and shrunk. It is no longer
"this can lose your data". It is the one documented trade from
[ADR-0064](0064-the-cross-node-cost-of-the-read-cache.md): a plain read can be up
to the TTL stale, so read-your-own-writes does not hold across nodes. That is a
semantic an application opts into, not a defect.

| the key is… | with `--kv-cache-ms` |
|---|---|
| read and written by the same node only | safe |
| written by one node, read by others | reads stale up to the TTL |
| written by more than one node | **safe** — the loser retries |
| never written | safe |

## Still open

- **Index maintenance is still read-modify-write.** `add_secondary_indexes` and
  its sibling are separate unguarded writes, so a tight interleaving on one index
  key can still drop or duplicate an id. That is a weaker failure than losing a
  record — the records are authoritative and `find-by` re-verifies against them —
  and it is the next thing to point this primitive at.
- **Every other component that hand-rolls a read-compare-write** has the shape
  ADR-0065 found. `gate-domain` has its own CAS loop over `records::update`, so it
  inherits the fix; anything talking to `wasi:keyvalue` directly does not.
