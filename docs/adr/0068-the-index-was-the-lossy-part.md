# 0068 — The index was the lossy part

Status: accepted, and built. Closes the last two durability gaps
[ADR-0067](0067-one-copy-is-not-a-backup.md) named, and reports a worse bug found
on the way to them.

## The id index could lose a record without losing it

`record-store` keeps each collection's ids in a sorted, chunked list, and
`ids_insert` was a read-modify-write over a chunk. Two concurrent inserts landing
in one chunk both read it, both rewrote it, and one id was gone.

Nothing noticed, and that is the point. `get` and `find-by` read records by key,
so the record was still there and still readable. Only `list`, `count` and
`query` page over the id index — so the record simply stopped being listed. To
anyone looking at the app it had vanished; to anyone looking at the store it was
fine. There was no way to notice and no way to put it back.

The chunk rewrite is now guarded by its revision through
[`comp:store/cas`](0066-the-guard-moves-into-the-store.md): a loser re-reads and
redoes its insert on top of the winner's. The manifest stays a plain write — it
holds routing metadata derived from the chunks, and `ids_read_all` concatenates
every chunk it names, so drift there costs ordering, never membership.

## `repair`, because "cannot happen again" is not the same as "is fixed"

Every index written before today may already have dropped an id, and a guard does
nothing about that. The records are authoritative and the index is an
acceleration layer over them, so the disagreement is always resolvable in one
direction: scan what exists, make the index say that.

```
records existing but unlisted   readded
ids named with no record        pruned
```

Exposed at `POST /api/internal/repair?collection=` for the platform's own
collections — the catalogue, orgs, deployments, the ones whose loss hurts most. A
tenant's app owns its own records in its own bucket and has to expose its own.

Measured, by dropping an id from a live index the way a lost write would:

```
index before          3 ids
drop one              the record is untouched, the index names 2
repair                {"pruned":0,"readded":1,"total":3}   <- it is back
repair again          {"pruned":0,"readded":0,"total":3}   <- converged
```

And in both directions at once, on an index that named two phantoms and had lost
all three real ids: `{"pruned":2,"readded":3,"total":3}`.

## The bug found on the way, which was worse than the one being fixed

The first version of `repair` reported `total: 0` on a collection with records,
and **pruned a perfectly good index down to nothing**.

`NatsKv::safe_key` escapes bytes NATS will not take as `_XX`, and leaves a
literal `_` alone. `unescape` then decoded `_XX` back. Those two cannot both be
right, because `_` is the escape introducer AND a legal character:

```
"rec_orgs_01KZS84N77"  -> stored "rec_orgs_01KZS84N77" -> read back "rec_orgs\x01KZS84N77"
```

Every record key is that shape, because a ULID starts with digits. So
`list-keys` on the NATS backend returned corrupted names — to `blob-store`,
`audit-log`, `feature-flags`, `cache`, `metrics-collect` and four others, not just
to `repair`.

There was a test asserting keys round-trip. It passed, because every example it
chose dodged the ambiguity: `a_b` has one character after the underscore, so the
decoder left it alone. It is replaced by two tests that pin what is actually true
— a key of legal characters is stored verbatim, and a key needing escapes comes
back escaped rather than decoded.

`list_keys` now returns keys exactly as stored and there is no decoder at all.
For every component in this repo that is byte-identical, because they sanitize
their own key segments. A key containing bytes that had to be escaped comes back
in escaped form: a wart, said out loud, and not corruption. Making the encoding
truly reversible means changing `safe_key`, which renames every key already
written — a migration, not a bug fix.

## And a repair that trusts its input is a liability

The near-miss above is a design lesson, not just a bug. `repair` now refuses to
act when the scan finds nothing while the index is populated:

```
repair ghosts: the scan found no records while the index names 1.
Refusing to rewrite it — this is a broken scan, not an empty collection.
```

A collection that lost every record at once is far less likely than a scan that
is broken, and the tool whose job is to recover data must not be the thing that
destroys it. Verified: the index was left exactly as it was.

## Still open

- **Secondary indexes** (`ix_…`) use the same guarded chunk writes now, but
  `repair` only rebuilds the id index. Rebuilding those means re-reading every
  record's indexed fields, which is the same scan and worth doing next.
- **A record and its indexes are still separate writes.** A crash between them
  leaves them disagreeing until someone runs `repair`. Detecting that
  automatically — a periodic verify — is the remaining piece.
