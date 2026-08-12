# 0071 — A captured fetch is spent

Status: accepted, and built. Closes the replay gap
[ADR-0051](0051-the-secret-reader.md) named and
[ADR-0061](0061-the-secret-reader-was-never-linked.md) left open, and finishes
`repair` from [ADR-0068](0068-the-index-was-the-lossy-part.md).

## Replay

ADR-0051 said it plainly and did nothing about it: *"there is no nonce or request
id, so a captured request can be REPLAYED against the platform until the token
expires."* TLS is the only thing that was stopping it, which means anyone who
could see one fetch could repeat it — for the token's whole life, as many times
as they liked.

A host now sends a timestamp and a nonce with every fetch. The platform refuses
a request outside a 60-second window, and **claims the nonce exactly once**:

```rust
match cas::set(&bucket, &key, b"1", 0) {          // 0 = must not exist yet
    Ok(cas::Outcome::Committed(_)) => Ok(()),      // first use
    Ok(cas::Outcome::Conflict(_))  => refuse,      // a replay
```

One guarded write whose **failure is the answer** — no lookup, no read-then-check
race, no index. This is the primitive from
[ADR-0066](0066-the-guard-moves-into-the-store.md) doing something other than what
it was built for, which is a good sign about the primitive.

Two deliberate choices:

- **A request with no nonce is refused, not waved through.** An old host is one
  whose requests can be replayed; accepting it would make this decoration. It
  does mean host and platform have to be upgraded together.
- **The nonce is unique, not unpredictable.** The attacker holds a request they
  already captured; guessing a *future* nonce gains them nothing, because the
  platform refuses one it has seen. Process id, nanoseconds and a counter cover
  every way two fetches can race, and needed no new dependency.

The window is what bounds the set of nonces to remember, and it is why the
timestamp is checked at all.

```
first use of a nonce        200
the same request again      409
a fresh nonce               200
no nonce at all             409
an hour-old timestamp       409
```

## `repair` now rebuilds the secondary indexes too

ADR-0068 rebuilt the id index and left the `ix_…` lookups out. That was half a
fix: `find-by` and `query` read those, so an id missing from one is a record that
exists, is listed, and **cannot be found by the field it is indexed on**. The
same silent invisibility, one layer down.

They are derived from the records, so recomputing is the check and the fix at
once. Index keys that no record points at any more are emptied as well — they
would otherwise over-match forever, costing a read per stale id on every lookup.

Measured by emptying a live index: the record is listed but unfindable, `repair`
reports `indexes: 3`, and the id is back where it belongs.

## Still open

- **Nothing sweeps old nonces.** They are small, keyed by window, and written at
  instance-start rates rather than request rates — but they accumulate. The key
  is `fn_{window}_{nonce}` precisely so a sweeper can drop a whole window by
  prefix, and that sweeper does not exist.
- **No in-transit wrapping.** Still TLS only, as ADR-0051 said. Replay is closed;
  an attacker who can read the transport still reads the plaintext.
