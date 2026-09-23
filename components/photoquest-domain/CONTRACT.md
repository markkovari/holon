# `photoquest:domain` — the contract

Upload a photo straight to object storage, have it evaluated on the machine with
the GPU, and see it back. This is step one of photoquest: upload, renditions and
a gallery. Quests, XP and levels come next and are not here — the seam they plug
into is `src/quests.rs` (below).

Why the bytes never pass through this component, and what `comp-media` is:
[ADR-0098](../../docs/adr/0098-photos-live-in-object-storage.md). The daemon's
side of every call below is [`media-pipeline/CONTRACT.md`](../media-pipeline/CONTRACT.md).

## Imports

| import | why |
|---|---|
| `auth:identity/*` | real accounts, login, roles (`guestauth` macros) |
| `records:store/store` | the `photos` collection, indexed by `owner` |
| `audit:log/recorder` | register/login, and every refusal |
| `media:pipeline/jobs` | upload plans, completion, the evaluation queue, signed URLs |
| `webhook:sign/signer` | verifying the evaluator's callback |
| `wasi:config/store` | the three keys below |

## Config

| key | meaning |
|---|---|
| `public-callback-base` | where `comp-media` can reach this app, e.g. `http://127.0.0.1:3941`. `/internal/photos/{id}/evaluated` is appended. **Unset: `complete` answers 503** before touching the upload |
| `media-callback-secret` | the HMAC key; must equal the daemon's `--callback-secret`. **Unset: every callback answers 503** (and is retried) — an unverifiable result is never written |
| `media-url`, `media-token` | read by `media-pipeline`, not by this component |

## Accounts

| | |
|---|---|
| `POST /register` | `{"email","password"}` → 201 `{subject, role}`. Everyone is a `photographer`; a `role` in the body is ignored |
| `POST /login` | → 200 `{access_token, refresh_token, expires_in}`; 401 `invalid_credentials` |
| `POST /logout` | bearer → 204 |
| `GET /me` | bearer → 200 `{subject, tenant, roles}` |
| `GET /health` | 200 `{"ok":true}` |

`admin` is honoured on reads when an operator assigns it through `auth:identity/rbac`,
and can never be asked for at registration: an admin reads every photo.

## Routes

Every `/api/**` route needs `Authorization: Bearer <access_token>`; without one it is
**401 `unauthorized`**.

| route | who | body | answer |
|---|---|---|---|
| `POST /api/photos` | any photographer | `{filename, size, content_type}` | 201 `{photo, upload}` — `upload` is the plan: `{upload_id, key, part_size, parts: [{number, url}], expires_at}` |
| `POST /api/photos/{id}/complete` | the owner only | `{parts: [{number, etag}]}` | 200 `{id, state: "processing"}` |
| `GET /api/photos` | any photographer | — | 200 `{photos: [...]}` — the caller's own, newest first, each with `thumb_url` (signed, or null), `sharpness.tiles` left out |
| `GET /api/photos/{id}` | the owner, or an admin | — | 200 the full record plus `urls: {thumb, share, ai}` (signed for an hour when `evaluated`, else null) |
| `POST /internal/photos/{id}/evaluated` | `comp-media`, by signature | the callback body | 200 `{id, state}` (`duplicate: true` when already settled) |

The browser `PUT`s each part to its `url` itself, reads the `ETag` response header,
and sends the list to `complete`. `complete` is safe to retry: from `uploading` it
completes the upload and submits; from `uploaded` or `processing` it only
resubmits, and the job id is the photo id, so `MEDIA_JOBS` drops the duplicate.

## The callback

Not behind a login. In order:

1. `media-callback-secret` unset → **503 `callback_secret_unset`**.
2. `X-Media-Signature` missing, or `signer::verify(raw body, header, secret, github, 0)`
   fails → **401 `bad_signature`** — before the photo is looked up, so an unsigned
   caller learns nothing about which ids exist.
3. Body not a JSON object → 400 `bad_json`; `photo_id` ≠ the path's id → 400;
   `status` not `done`/`failed` → 400.
4. No such photo → 404. `job_id` ≠ the photo's job id → 409 `job_mismatch`.
   Photo still `uploading` → 409 `not_submitted`.
5. Photo already `evaluated`, or already `failed` and this is `failed` → 200
   `duplicate: true`, **nothing written**. A late `failed` never undoes an evaluation.
6. Otherwise `backend, sha256, metadata, renditions, sharpness, vision, colour,
   timings_ms` are copied onto the record, `state` becomes `evaluated` (`error: null`)
   or `failed` (`error` from the body), `evaluated_at` is stamped, and — for
   `evaluated` only — `quests::on_evaluated(&photo, &result)` runs.

`on_evaluated` therefore runs exactly once per photo, on the transition into
`evaluated`. It does nothing yet.

## The record (`photos`)

```jsonc
{
  "owner": "<principal.subject>",      // indexed
  "filename": "DSC01234.ARW", "size": 129581056, "content_type": "image/x-sony-arw",
  "state": "uploading",                // see below
  "created_at": 1790000000,
  "upload_id": "…", "key": "originals/<id>.arw",   // from the plan
  "uploaded_at": 1790000100,           // after complete-upload
  "job_id": "<id>", "submitted_at": …, // after submit
  "evaluated_at": …, "error": null,    // after the callback
  "backend": {…}, "sha256": "…", "metadata": {…}, "renditions": {…},
  "sharpness": {…}, "vision": {…} | null, "colour": {…}, "timings_ms": {…}
}
```

The record id (a ULID) is the photo id, the job id, and the `<photo_id>` in every
object key. Answers merge it in as `id`.

## States

    uploading ──complete──▶ uploaded ──submit──▶ processing ──callback──▶ evaluated
                                                            └──callback──▶ failed

`uploaded` is where a photo rests when `complete-upload` succeeded and `submit` did
not; retrying `complete` moves it on.

## Errors

| status | `error` | when |
|---|---|---|
| 400 | `bad_json`, `filename is required`, `size is required`, `parts is required`, … | malformed request |
| 401 | `unauthorized`, `invalid_credentials` | no or bad bearer / login |
| 401 | `bad_signature` | callback not signed with `media-callback-secret` |
| 403 | `forbidden` | not your photo (audited) |
| 404 | `not_found` | no such photo, or an id that is not `[A-Za-z0-9_-]{1,64}` |
| 409 | `already_evaluated`, `not_submitted`, `job_mismatch` | wrong state for the request |
| 409 | `media_not_found` + `detail` | the store has no such upload (expired, or already completed) |
| 413 | `body_too_large` | over 1 MiB |
| 422 | `media_refused` + `detail` | the daemon refused — too big, wrong type. A refused `POST /api/photos` leaves no record behind |
| 503 | `media_unavailable` + `detail` | the daemon or the store is down; retry |
| 503 | `public_callback_base_unset`, `callback_secret_unset` | the deployment is not finished |
