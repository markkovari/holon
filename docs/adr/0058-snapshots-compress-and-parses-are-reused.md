
# 0058 — Snapshots compress, parses are reused, and the watch was not built

Status: accepted. Closes the inventory half of
[ADR-0055](0055-how-the-control-loop-scales.md).

ADR-0055 named two walls in the inventory path: a node's snapshot approaching
NATS' 1 MiB message limit, and the reconciler reading and parsing the entire
world on every pass. Both are addressed. A third fix — replacing the poll with a
watch — was measured and deliberately not built.

## Snapshots are compressed

A snapshot is the largest and most frequent message the platform sends: every
node, every heartbeat, the whole truth about what it runs. It is JSON with six
field names repeated per instance and a 71-byte digest string that is usually one
of a handful, so it compresses about ten to one.

`zstd` at level 1, in the lattice's `Inventory` impl, so both sides are unaware
of it. The ceiling moves from ~5 500 instances per node to roughly 50 000, and a
thousand-node fleet on a five-second heartbeat drops from ~72 MB/s of bus traffic
to about seven.

**Detected by zstd's frame magic**, not by a version flag. JSON always starts
`{`, so the two can never be confused, and a fleet mid-rollout keeps working with
some nodes compressing and some not. That is the lesson of
[ADR-0044](0044-subjects-carry-a-version.md) applied without needing a new
version number.

Compression was chosen over designing a delta protocol because it is
transparent: no sequence numbers, no resync, and no way for the two sides to
disagree about what they have seen.

## Parses are reused

A node's snapshot is byte-identical between passes unless that node did
something. Re-parsing 20 000 instances of JSON to discover that costs 4.4 ms of a
45 ms pass; hashing the bytes and reusing the previous parse costs **0.6 ms**.

The reconciler keeps the parsed world and updates only the entries whose bytes
changed. Two properties are load-bearing:

- **An unreadable snapshot keeps the previous good one.** Same instinct as
  refusing a whole pass on an unreadable manifest rather than reading it as a
  deletion.
- **Absence is never cached.** A key gone from the bucket has expired, and that
  is the only liveness signal in the system — it is re-derived from the listing
  every pass, deliberately, while everything else is cached.

## The watch was measured and not built

The obvious version of this is a JetStream KV `watch()` keeping a mirror, with
the pass cost becoming proportional to change rather than to fleet size. It was
not built, and the reason is worth recording because it will be proposed again.

With snapshots compressed, the bandwidth argument was already gone. What
remained was the 4.4 ms parse — and a watch buys that in exchange for
**re-deriving node liveness locally**. Today `read_all` returns what has not
expired, so a node's absence *is* its death and there is no liveness logic
anywhere in the platform. That is the property that deleted all of ADR-0016's
orphan-reaping apparatus. A watched mirror would need its own expiry, plus a
periodic resync in case it missed an event, and would introduce a second way to
be wrong about which machines are alive.

Four milliseconds is not worth a second opinion about whether a machine is dead.
The hash gets the same saving and adds nothing that can disagree.

## Bounds

- zstd level 1 is a guess sized for "runs on every node every heartbeat". Nobody
  has measured the CPU cost of the compression against the bytes it saves.
- The reused parse is keyed on a `DefaultHasher` of the raw bytes. A collision
  would mean a stale snapshot; at 64 bits and a few thousand entries this is far
  below every other failure rate here, but it is a trade and not an absence of
  one.
- The 50 000-instance ceiling is inferred from a 10:1 ratio on one snapshot
  shape. A fleet whose instances share fewer digests compresses worse.
