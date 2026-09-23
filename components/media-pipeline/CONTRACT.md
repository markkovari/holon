# media-pipeline ↔ comp-media contract

`media-pipeline` (this component) exports `media:pipeline/jobs` and implements every
call as one HTTP request to `comp-media` (native, `reconciler/src/bin/media.rs`).
Why the split exists: [ADR-0098](../../docs/adr/0098-photos-live-in-object-storage.md).

## Config the component reads (`wasi:config`)

| key | meaning |
|---|---|
| `media-url` | where `comp-media` listens, e.g. `http://127.0.0.1:8013` |
| `media-token` | bearer token; must match the daemon's `--token` (absent = no header) |

## Daemon HTTP API

All requests and responses are JSON. Every route except `GET /health` requires
`Authorization: Bearer <token>` when the daemon has one (`comp_reconciler::daemon_auth`).
Errors are HTTP 200 with `{"error": "refused"|"not-found"|"unavailable", "detail": "…"}` —
the same convention as `comp-imageopt` — so a non-200 always means transport.

| route | body | answer |
|---|---|---|
| `POST /uploads` | `{photo_id, filename, size, content_type}` | `{upload_id, key, part_size, parts: [{number, url}], expires_at}` |
| `POST /uploads/complete` | `{upload_id, key, parts: [{number, etag}]}` | `{key}` |
| `POST /uploads/abort` | `{upload_id, key}` | `{}` |
| `POST /jobs` | `{job_id, photo_id, key, callback_url}` | `{job_id, queued: true}` |
| `POST /sign` | `{key, ttl_secs}` | `{url}` |
| `GET /health` | — | `{ok: true, store: bool, queue: bool, apple: bool}` |

Rules:

- **Keys.** Originals: `originals/<photo_id>.<ext>` (ext lower-cased from the filename).
  Renditions: `renditions/<photo_id>/{thumb,share,ai}.jpg`. `photo_id` must match
  `[A-Za-z0-9_-]{1,64}` or the call is `refused` — it becomes part of a key.
  A key's first segment names the bucket and the rest is the object in it:
  `originals/p1.arw` is object `p1.arw` in `--bucket-originals`. Any key not of
  exactly these two shapes is `refused` by every route.
- **`job_id`** follows the same `[A-Za-z0-9_-]{1,64}` rule (it names the job's temp
  directory); a UUID fits. `/jobs` also refuses a `key` that is not
  `originals/<that photo_id>.<ext>` and a `callback_url` that is not `http(s)`
  or whose `host:port` (port defaulted from the scheme) is not one of the
  daemon's `--callback-allow` entries. The worker POSTs to that URL from inside
  the network, so an open list would be an SSRF; with no `--callback-allow` at
  all every job is refused (logged loudly at startup). The worker re-checks a
  job it pulls, since anything with NATS access can publish to the subject.
- **Endpoints.** The daemon talks to the store at `--s3-endpoint`; every URL a
  browser is handed (upload parts, `/sign`) is signed against
  `--s3-public-endpoint` (default: the same). SigV4 signs the host, so a URL
  cannot be rewritten to another name afterwards.
- **Size / type.** `size` over `--max-upload-mb` (default 512) is `refused`; so is a
  `content_type`/extension not in the allow-list (initially `arw`, `jpg`, `jpeg`).
  Accepted `content_type`s: `""`, `application/octet-stream`, `image/jpeg`,
  `image/x-sony-arw`, `image/arw` — browsers send `""` or octet-stream for an ARW.
- **Parts.** `part_size` is 16 MiB (S3's minimum is 5 MiB); the last part is smaller.
- **`/sign`** signs only `renditions/` keys unless the daemon runs with
  `--sign-originals`; an original is not for browsers by default. `ttl_secs` is
  clamped to 1..=604800 (SigV4's limit). It does not check the object exists.
- **`/jobs`** publishes to the JetStream work-queue stream `MEDIA_JOBS` with
  `Nats-Msg-Id: <job_id>`, so a duplicate submit inside the dedupe window is dropped.

## The callback

When a job finishes (or finally fails), the worker `POST`s JSON to `callback_url` with

    X-Media-Signature: sha256=<hex HMAC-SHA256 of the raw body, key = --callback-secret>

— exactly the GitHub scheme of `webhook:sign`, so the receiver verifies with
`signer::verify(body, header, secret, scheme::github, 0)`. A non-2xx answer is
retried with backoff; the job is acknowledged only after a 2xx or after the last
attempt, and then a final `status: "failed"` is sent.

```jsonc
{
  "job_id": "…", "photo_id": "…",
  "status": "done",                     // or "failed"
  "error": null,                        // string when failed
  "backend": { "sharpness": "metal", "develop": "coreimage", "vision": true },
                                        // cpu | metal, rawler | coreimage | embedded-preview | jpeg, bool
                                        // (`jpeg`: CPU path, the original was a JPEG)
  "sha256": "…",                        // of the original, hex
  "metadata": {
    "camera": "Sony ILCE-7RM5", "lens": "FE 70-200mm F2.8 GM OSS II",
    "captured_at": "2026-09-23T13:11:47",  // camera clock, no zone
    "exposure_s": 0.004, "fnumber": 4.0, "focal_mm": 200.0, "iso": 6400,
    "width": 9504, "height": 6336
  },
  "renditions": {
    "thumb": { "key": "renditions/<id>/thumb.jpg", "width": 512,  "height": 341,  "bytes": 41210 },
    "share": { "key": "renditions/<id>/share.jpg", "width": 4096, "height": 2731, "bytes": 2874394 },
    "ai":    { "key": "renditions/<id>/ai.jpg",    "width": 1568, "height": 1045, "bytes": 323338 }
  },
  "sharpness": {
    "method": "green-stab-v1",          // sqrt -> 3x3 binomial -> 4-neighbour Laplacian, per tile
    "grid": 16,
    "tiles": [ /* 256 variances x1e6, row-major, top-left origin */ ],
    "floor": 128.7,                     // median tile
    "peak": 912.3,                      // max(tile - floor)
    "focus_ratio": 6.1,                 // peak / floor
    "subjects": [                       // one per Vision face (empty without Vision)
      { "kind": "face", "box": [0.23, 0.21, 0.18, 0.27], "sharpness": 59.1, "ratio": 0.5 }
    ]
  },
  "vision": {                           // null when no Vision stage ran
    "labels": [ { "id": "people", "confidence": 0.96 } ],
    "faces": [ { "box": [0.23, 0.21, 0.18, 0.27], "quality": 0.55 } ],
    "attention": [ [0.14, 0.11, 0.39, 0.77] ],
    "objectness": [ [0.0, 0.06, 1.0, 0.94] ],
    "aesthetics": { "overall": 0.688, "utility": false },
    "horizon_deg": -9.63                // unreliable indoors — informational only
  },
  "colour": { "mean_luma": 0.41, "clipped_shadows_pct": 0.3, "clipped_highlights_pct": 0.1, "saturation_mean": 0.22 },
  "timings_ms": { "download": 900, "decode": 15, "sharpness": 3, "develop": 1350, "vision": 550, "upload": 400 }
}
```

All boxes are `[x, y, w, h]`, normalised 0–1, **top-left origin** (Vision's
bottom-left origin is converted in the helper). Sharpness tiles are in the same
frame: the green plane is cropped to the picture and turned upright (EXIF
orientation) before either backend sees it.

A `failed` body carries `job_id`, `photo_id`, `status`, `error`, and every other
field `null`. For a JPEG original the `metadata` fields other than `width`/`height`
are `null` (EXIF of JPEGs is not read yet). Renditions carry no EXIF — the
camera's serial number and GPS stay out of a picture meant to be shared.

## The Swift helper (`comp-media-apple`)

Built from `tools/media-apple/main.swift` with `swiftc -O`. `comp-media` runs it
per job when `--apple-helper <path>` is set and the binary exists:

    comp-media-apple --original <file.arw> --green <plane.f32> --green-width W --green-height H --out <dir>

It writes `<dir>/{thumb,share,ai}.jpg` and prints one JSON object on stdout with
`renditions` (`width`/`height`/`bytes`; the daemon adds `key`), `sharpness` (Metal,
same method, including `subjects`; `null` when there is no Metal device, and the
daemon then computes it on the CPU), `vision`, plus `develop` (`"coreimage"`) and
`timings_ms` (`develop`, `vision`, `sharpness`) which the daemon merges into its own.
A non-zero exit or unparseable output makes the daemon fall back to the CPU path.
Without the helper (or off macOS) the daemon does it itself: `rawler` develops the
renditions, sharpness runs on the CPU, and `vision` is `null`.
