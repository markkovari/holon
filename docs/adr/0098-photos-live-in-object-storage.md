# ADR-0098 — Photos live in object storage, and evaluation is a queue on the machine with the GPU

*A 61-megapixel raw file is 129 MB. Nothing in the runtime path could carry one, so
the bytes go where bytes belong and everything else carries keys.*

**Status: accepted.** First user: `photoquest` (a gamified photo-evaluation app).
Adds one native daemon, `comp-media`, under the rule in
[0095](0095-what-is-allowed-to-be-native.md).

## What could not carry the file

Measured against a Sony a7R V uncompressed ARW (9600×6376, 129,581,056 bytes):

| layer | limit | where |
|---|---|---|
| guest memory | 64 MiB, and the pooling allocator hardcodes it | `host/src/main.rs` `--mem-cap-mb`, `pool.max_memory_size` |
| guest body read | every image app stops at 16 MiB | `guestio::guest_read_body!` callers |
| `blob:store` | whole `list<u8>`, stored as a KV value | `components/blob-store` |
| NATS KV value | `max_payload`, 1 MB by default | `host/src/kv.rs` `NatsKv` |

comp-host streams the request body into the guest; everything after that buffers.
The file cannot pass through a component, and it would be wrong to make it: a
component's job here is deciding who may upload what, not moving 129 MB.

## The decision

1. **Bytes live in S3-compatible object storage, from the first upload.** Originals
   in an `originals` bucket (written once, read by the evaluator, then cold);
   renditions — thumbnail, web share, AI copy — in `renditions`. Nothing large is
   ever a KV value or a NATS message. Moving originals out of NATS later would be a
   migration with no benefit, so they never go in.
2. **The contract is the S3 API, not a product.** The default is RustFS
   (Apache-2.0, 1.0 GA September 2026) in `infra/compose.yaml`; Garage, SeaweedFS,
   Backblaze B2 or R2 are configuration. MinIO's community edition is in
   maintenance mode (December 2025) and is not the default for anything new.
3. **The browser uploads directly, by presigned multipart.** A component asks for an
   upload plan; the browser `PUT`s parts straight to the store and reports the
   ETags; the component completes it. No process of ours carries the bytes in.
4. **`comp-media` is native, and owns a JetStream work queue.** It signs URLs, and a
   worker drains `MEDIA_JOBS` one photo at a time — the GPU is the bottleneck, not
   the queue. Guests cannot publish to NATS (comp-host deliberately offers no
   messaging), so the queue belongs to the daemon and a component enqueues over HTTP.
5. **Evaluation happens on-device.** Decode with `rawler`; on macOS, develop with
   Core Image, look with the Vision framework, and compute sharpness with a Metal
   kernel, through a small Swift helper. Elsewhere, the same metrics on the CPU and
   renditions from `rawler`'s own developer. No cloud model by default.
6. **Results come back as a signed callback**, verified with `webhook:sign`. The
   component scores and stores them inside that request — it has no background.

## Why native passes 0095

A `wasm32-wasip2` component cannot hold a JetStream consumer, cannot reach Metal,
Core Image or Vision, and cannot hold a 129 MB file. All three are the daemon's
whole job. It is one daemon for one capability, behind a WIT contract
(`media:pipeline`), reached by a thin component the way `image-optimizer` reaches
`comp-imageopt`, so `HOST_IFACES` stays identical on every node.

## What the spike measured (M2 Max, two real a7R V files)

| stage | time |
|---|---|
| `rawler` decode / metadata / embedded preview | ~15 / ~50 / ~20 ms |
| green plane (half-res, straight from the Bayer data) | ~50 ms |
| sharpness: CPU / **Metal** (identical tile values) | ~80 / 1–3 ms |
| Core Image develop + three renditions | ~1.35 s (4096 px share: 2.3–2.9 MB) |
| Vision: classify, faces, saliency, aesthetics | ~0.55 s |

Two findings shaped the metric, not just the plan:

- **A raw Laplacian variance scores sensor noise as sharpness.** At ISO 6400 the
  "sharpest" tile was an out-of-focus white shirt. The metric that works: square
  root first (shot noise grows with √signal), a 3×3 binomial blur, then subtract
  the frame's own noise floor (the median tile). That is `green-stab-v1`.
- **Sharpness belongs where the subject is.** Under each Vision face box the
  in-focus face scored 59 and 108 against 0.5 and 1.5 for the other one; Vision's
  own `faceCaptureQuality` ranked the two faces the wrong way round in one frame.
  Horizon detection reported −9.6° indoors, where there is no horizon — it counts
  only for landscape quests.

## Consequences

- The upload path needs CORS on the bucket (the browser reads each part's `ETag`).
- A non-Mac node evaluates with CPU sharpness and no Vision stage; results say
  which backend produced them, so a quest can require the stage it needs.
- `comp-media` can run on a different machine from the store — it reads by key.
- Quests, XP and levels are domain code in `photoquest-domain`, not part of this.
