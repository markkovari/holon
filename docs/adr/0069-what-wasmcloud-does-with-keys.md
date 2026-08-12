# 0069 — What wasmCloud does with keys: nothing

Status: accepted. Read their provider instead of inventing, and the answer
mostly validated [ADR-0068](0068-the-index-was-the-lossy-part.md) — with one
thing worth taking.

## The question

ADR-0068 found that this host's NATS key encoding is not reversible: `safe_key`
escapes illegal bytes as `_XX` and leaves a literal `_` alone, so `list-keys`
handed back corrupted names. The obvious move was to stop inventing and copy
whoever had already solved it — wasmCloud runs the same components against the
same `wasi:keyvalue` contract on the same NATS.

Source read: `crates/provider-keyvalue-nats/src/lib.rs` at `v1.5.0`.

## What they do: no encoding at all

```rust
store.purge(key.clone()).await          // delete
store.entry(&key).await                 // get
store.keys().await … try_collect()      // list_keys, returned verbatim
```

The guest's key goes straight to NATS. There is no escape function and no decode
function, so there is nothing to get wrong — and a key NATS will not accept
simply fails, with the error handed back to the component.

That is a different trade from ours, not a better implementation of the same one:

| | wasmCloud | here |
|---|---|---|
| a key of legal characters | works | works, byte-identical |
| a key with a space, `/`, `:` | **rejected by NATS** | escaped, works |
| `list-keys` | exact, because nothing was encoded | exact for legal keys, escaped form otherwise |

**Reject** is simpler and cannot corrupt. **Escape** accepts more keys and, done
the way it was here, could not be undone. Neither is free, and switching to
rejection now would break every key already written that contains an escaped
byte — a migration, and one that turns working keys into errors.

So the encoding stays, and ADR-0068's decision — hand back what is stored, never
decode — is confirmed as what the mainstream implementation does anyway. That
part needed no change; it needed the confidence that comes from checking.

## What was worth taking

Their `increment` is a compare-and-set retry loop against `entry.revision`, the
same shape as ours. One difference:

```rust
const EXPONENTIAL_BACKOFF_BASE_INTERVAL: u64 = 5; // milliseconds
```

Ours retried immediately, thirty-two times. Under contention on one key that is a
thundering herd: every loser re-reads and re-collides at full speed, and the
harder the contention the worse it gets. Now backed off 5ms, 10ms, 20ms… capped,
with the first attempt still immediate so the uncontended path is unchanged.

Adopted. **Then measured, and it is not the win this sentence originally
claimed** — see `bench/FLEET-BENCH.md`. On one hot key at c=20 across a
three-machine quorum, throughput is unchanged within noise; what backoff buys is
the tail (p99 7 490 ms → 4 007 ms) at the cost of the median (p50 1 095 ms →
1 737 ms). A defensible trade, and a different one from "prevents a retry storm".
The original claim came from reading their source rather than from measuring this
fleet, which is exactly the mistake this repo keeps writing ADRs about.

## What could not be taken, and why it matters

Their provider **also has no compare-and-set to offer a component**. It uses
revisions internally for `increment` — the one atomic operation
`wasi:keyvalue@0.2.0-draft` defines — and there the contract stops:

```
store    get · set · delete · exists · list-keys
atomics  increment
batch    get-many · set-many · delete-many
```

So the lost update in [ADR-0065](0065-the-cache-defeats-the-revision-guard.md)
was never a wasmCloud-versus-here question. Any component doing
read-compare-write over that interface has it, on either runtime, and
[`comp:store/cas`](0066-the-guard-moves-into-the-store.md) exists because the
contract does not carry one. Reading their source is what makes that a measured
statement rather than an assumption.

The same goes for the thing that caused all of this: `record-store`'s typed
records, revisions and secondary indexes have no wasmCloud counterpart to copy.
That layer is ours, and so are its races.

## Also noticed, not adopted

They `purge` on delete where this host calls `delete`, which leaves a tombstone.
With `history: 1` both are close to equivalent and the CAS path here already
treats an empty entry as absent. Worth revisiting if tombstones ever show up in a
`list-keys` result; not worth a behaviour change today.
