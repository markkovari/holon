# ADR-0101 — State can live somewhere else: object bytes on S3/R2, and backups off the box

**Status: accepted, and built.** `--blob s3` on `comp-host` (`host/src/objkv.rs`),
`blob-store` writing its bytes to a second host-assigned store, and `comp-backup`
(`reconciler/src/bin/backup.rs`). Gated by `host/src/objkv_test.rs`,
`reconciler/tests/blob_s3.rs` and `reconciler/tests/backup.rs`, all against real
servers.

## The question

"Can holon keep its state remotely — in Cloudflare's object storage, say?"
Part of it already could. `--kv nats --nats-url <anywhere>` and `--kv redis
--redis-url <anywhere>` point every component's keyed state at a store on
another machine, and a 3-node JetStream cluster is the distributed option,
measured losing a machine under load (ADR-0067, `bench/FLEET-BENCH.md`).

What could not: an **object store** as the home of anything, and a copy of
the state **off the machines that hold it**.

## Why keyed state stays where it is

An object store cannot be the `--kv` backend in practice, and not because of
correctness — S3 and R2 both enforce conditional writes now, so a real
compare-and-set is buildable (and is built, below). It is latency: one S3
round trip is 20–100 ms over a WAN, and ADR-0070 counted 85 store operations
in one request before it was fixed down to 2. Over Tailscale alone the same
request touching no storage already dropped from 41 707 rps to 1 230. Keyed
state belongs on a store that answers in under a millisecond.

Object bytes are the opposite shape: large, read and written whole, a few
times per request at most. That is the one kind of state an object store is
the right home for.

## The decision

### 1. A bucket has a class, and the host assigns it

`BucketId` carries a `StoreClass` — `Kv` or `Object` — minted in the same place
as its name (`tenant.rs`, ADR-0012's boundary). A scope now grants two stores:
`default` (keyed state, `b-<env>`) and `blobs` (object bytes, `o-<env>`).
`RoutedKv` sends `Object` buckets to the object backend and everything else to
`--kv`. A guest string still chooses nothing: it is a key into the host's map,
and the class came with the entry.

Routing by bucket, not by key prefix, because a composed app has **one**
`wasi:keyvalue` import shared by every component in it; the host cannot tell
`blob-store`'s calls from anyone else's, and matching on `bo_` would couple the
host to one component's key scheme.

### 2. `blob-store` puts bytes in `blobs` and its index in `default`

The metadata key is the commit point: written after the bytes, deleted before
them. A crash between leaves unreferenced bytes, never a listed object with
nothing behind it. `exists` reads the index (the fast store). On a host that
grants no `blobs` store — one older than this — everything stays in `default`
as before, and reads fall back to `default` for objects written before the
split.

### 3. `--blob s3`: a full `KvBackend` over one S3 bucket

Each host bucket is a key prefix. The revision rides in `x-amz-meta-rev`, and
**every** write, a plain `set` included, is a conditional PUT against the ETag
it read (`If-None-Match: *` to create, `If-Match` to replace), so two writers
can never land one revision. That rests on the store enforcing those headers —
so `S3Kv::connect` **proves it** at startup with four writes to a scratch key
and refuses to start on a store that answers `200` to either refusal. RustFS
1.0.0 passes; AWS S3, R2 and MinIO document the headers.

Measured on RustFS: exactly one of eight racing compare-and-sets lands;
six threads × ten increments on one key reach 60. The first version retried a
lost race immediately and starved a writer past 32 retries one run in four;
jittered exponential backoff fixed it (20 of 20 since).

`reconciler/tests/blob_s3.rs` deploys `virt-git` → `blob-store` with `--blob s3`,
commits a 2 MB file (over NATS's default 1 MB payload), reads it back through
the linked capability, and then lists **the bucket itself**: every key is under
an `o-` prefix and is a `bo_` data key, and one object holds the 2 MB. Refs stay
on NATS.

### 4. `comp-backup`: every stream and named SurrealDB, sealed, to a bucket

ADR-0067 built backup on `nats stream backup` — the vendor's snapshot — and
said writing our own would reimplement a wire format for nothing. Those
`just backup` / `just restore` recipes were later lost (the tree has no
justfile), so today there is no backup at all. `comp-backup` replaces them,
and differs from that ADR on purpose:

| | `nats stream backup` | `comp-backup` |
|---|---|---|
| needs | the `nats` CLI on the machine | nothing but this binary |
| goes to | a local directory | any S3-compatible bucket, off-site |
| sealed | no | ChaCha20-Poly1305, by default |
| SurrealDB | no | `--surreal-db ns/db` |
| retention, list | no | `--keep N`, `list` |
| sequences, timestamps | **preserved exactly** | renumbered and restamped |

The last row is the cost, and it is stated in `streams.rs`: a restored KV
revision differs from the original, and anything holding one across the
restore must re-read — which every CAS caller does on a mismatch anyway.
Guard headers (`Nats-Expected-*`) and dedup ids (`Nats-Msg-Id`) are dropped on
replay, because the stored message still carries them and they would refuse
the write against a renumbered stream. The test proves that matters: with the
stripping removed, a bucket whose history limit left a gap fails to restore.

The format is one JSONL line of stream config, then one per message (subject,
headers, base64 bytes), gzip'd, then sealed in 1 MiB chunks whose AAD binds the
part's path in its backup, its index and a last-chunk flag — so a part cannot
be swapped, reordered, truncated or extended. The manifest (sizes, SHA-256s,
the key's fingerprint) is written last and is the commit point. A work-queue
stream refuses a second consumer, so it is read by sequence instead.

The e2e test backs up KV history with a delete marker and a guarded update, a
3 MB object store, headers, a work queue and a SurrealDB database; restores
into servers that never saw them; and checks the refusals — an unsealed run
nobody asked for, an existing stream without `--replace`, the wrong key
(caught from the manifest), a part the bucket altered, and `--keep 2`.

## Not done

- **Mirroring an S3 bucket elsewhere.** Bytes under `--blob s3` are already in
  a durable remote store; replicating it is the provider's tooling.
- **`--kv sqlite` files** are node-local by design and not in a backup.
- **Point-in-time across stores.** Streams are read one after another; vcs is
  written to be repaired from either half lagging, so run its `repair` after a
  restore.
- **Not run against R2 itself.** Everything above is against RustFS; R2 needs
  `--s3-region auto` and an account, and the startup check is what will say
  whether it behaves.
