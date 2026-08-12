# 0075 — Silence is not health

Status: accepted, and built. Closes the detection half of
[ADR-0068](0068-the-index-was-the-lossy-part.md) and
[ADR-0071](0071-a-captured-fetch-is-spent.md), which built `repair` and left
nothing to tell you when to run it.

## The gap

`repair` rebuilds an index from the records. It only ever runs when a human
suspects something — and the failure it repairs is, by construction, the kind
nobody suspects: a record that exists, is readable by id, and has quietly stopped
appearing in listings. The system looks fine. `CURRENT.md` has said so plainly
since ADR-0068: *"nothing detects that automatically."*

## Two halves, and only one of them is free

**Dangling entries — the index names a record that is gone.** The read path
already discovers these: `list` pages the index, fetches the records, and gets
back fewer than it asked for. That work was already happening and the answer was
already known; it was simply thrown away. Now it says so:

```json
{"drift":true,"collection":"catalog","op":"list","unresolved":1,"fix":"records:store repair"}
```

Shaped like `audit-log`'s output on purpose, so an existing scrape picks it up
with no new plumbing. A component instance is per-request (ADR-0037), so there is
nowhere to keep a counter — one line per occurrence is the only honest option,
and a healthy system prints nothing.

**Records the index never mentions — the direction that hides data.** A read
cannot see this. It cannot miss what it was never told to look for. Only a scan
finds it, so `verify` is that scan: the same comparison `repair` makes, changing
nothing.

```
GET /api/internal/verify?collection=orgs

{"clean":true,  "records_unindexed":0, "index_entries_dangling":0, "stale_index_keys":0, "total":3}
{"clean":false, "records_unindexed":0, "index_entries_dangling":1, ...}   <- after damage
{"clean":false, ...}                                                      <- asked twice, still broken
```

Reported, and reported *again* — a verify that quietly fixed things would be a
repair with a misleading name, and the second call is the assertion that it is
not. `repair` then prunes the phantom and `verify` goes clean.

The fields are named for what repair WOULD do rather than what it did:
`records_unindexed`, not `readded`.

## What is still missing, and it is the important part

**Nothing runs `verify` on a schedule.** It is a question anyone can ask and
nobody is asking. A cron, a periodic pass in the reconciler, or a check folded
into an existing loop would close it — and I am not going to pretend an endpoint
is monitoring.

So the honest state is: the cheap half is automatic, the expensive half is
available, and turning "available" into "noticed" is one scheduler away.

## A note on where this belongs

The drift line comes from `record-store`, which is a component in a tenant's
graph — so it lands in that tenant's host log, not in a platform-wide one. That
is correct (the platform cannot read a tenant's records) and inconvenient (nobody
aggregates tenant host logs yet). `verify` on the internal API covers the
platform's own collections; a tenant's app has to expose its own, exactly as with
`repair`.
