# `photoquest:domain` — the contract

Upload a photo straight to object storage, have it evaluated on the machine with
the GPU, and see it back — then play with it: quests in journeys with their own
levels, timed competitions, and moderation. The first half of this file is the
photo lifecycle; "The game" below is everything built on top of it.

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

---

# The game (step two)

Everything below is the contract for quests, journeys, levels, timed competitions
and moderation. Three decisions shape it (made 2026-09-24):

1. **Competitions are scored by a mix** of automatic metrics, photographer votes
   and curator judging, weighted per competition.
2. **Levels are per journey.** Each journey carries its own XP thresholds.
3. **Any photographer can report** a photo they can see.

## Roles

| role | who | can |
|---|---|---|
| `photographer` | everyone; what register grants | upload, submit to quests, enter competitions, vote, report |
| `curator` | granted by an admin | create/edit/publish journeys, quests, competitions; judge competitions |
| `admin` | the operator, or granted by an admin | moderate (reports, hide/unhide, suspend), grant/revoke `curator`/`admin` |

Roles are additive (a curator is still a photographer). An admin is **not**
implicitly a curator; an admin can grant themself the role. The first admin is
the account whose email equals config `bootstrap-admin-email` (unset = none) —
granted at register, audited. No role can be asked for at register.

## Journeys, quests, levels

**Every quest belongs to exactly one journey** (a one-quest journey is fine), so
XP and levels always live in a journey — no global curve.

```jsonc
// journeys
{ "title": "Into the park", "description": "…", "state": "draft|published|archived",
  "created_by": "<curator>", "created_at": …,
  "levels": [ { "level": 1, "xp": 0 }, { "level": 2, "xp": 100 }, { "level": 3, "xp": 250 } ],
                                       // ascending xp, first is always { 1, 0 }
  "badge": { "name": "Park ranger" },  // granted when every quest in it is passed
  "quests": ["<quest id>", …] }        // ORDER is the unlock order

// quests
{ "journey": "<journey id>", "title": "Something green", "description": "…",
  "xp": 50, "starts_at": 1790000000, "ends_at": null,   // unix secs; null = open-ended
  "requirements": { … see below … }, "state": "draft|published|archived", "created_by": "…" }
```

A photographer sees only `published` journeys/quests. Quest *n+1* of a journey is
**locked** until quest *n* is passed by that photographer. Editing a published
quest's requirements is refused (`409 quest_published`) — archive and replace it,
so a verdict always matches the rules it was judged by.

### Requirements (all optional; absent = not checked)

```jsonc
{
  "subject":   { "label": "grass", "min_confidence": 0.5 },   // a Vision label; or
  // "subject": { "face": true },                               // at least one face
  "sharpness": { "min_focus_ratio": 5.0, "min_subject_ratio": 0.5 },
                                       // focus_ratio from comp-media; subject_ratio =
                                       // best face `ratio` (needs a face)
  "aesthetics_min": 0.4,
  "exposure":  { "max_fnumber": 2.8, "max_shutter_s": 0.001, "min_focal_mm": 85,
                 "max_focal_mm": null, "max_iso": 3200 },
  "format":    "raw",                  // "raw" = an ARW original; absent = any
  "captured_after_start": true         // default true: metadata.captured_at >= starts_at
}
```

**Verdict** — every check reports itself, pass or not:

```jsonc
{ "pass": false,
  "checks": [ { "name": "subject", "ok": true,  "detail": "grass 0.90 ≥ 0.50" },
              { "name": "sharpness.min_focus_ratio", "ok": false, "detail": "3.1 < 5.0" },
              { "name": "aesthetics_min", "ok": null, "detail": "needs Vision; this photo was evaluated without it" } ] }
```

`ok: null` means the stage that produces the value did not run (`backend.vision`
false, or no `captured_at` on a JPEG); it counts as **not passed**, and the detail
says why, so a photographer is never told "bad photo" when the truth is "not looked at".
`captured_at` is the camera clock with no zone; it is compared as UTC.

### Submitting, XP, levels, badges

`POST /api/quests/{id}/submissions {photo_id}` judges the photo now and stores the
verdict. Refused before judging: not your photo (403), not `evaluated` (409
`not_evaluated`), hidden by moderation (409 `photo_hidden`), quest locked / not
started / ended / not published (409 with that reason), suspended account (403
`suspended`).

- A passing verdict awards the quest's `xp` **once per (photographer, quest)**.
- **A file (by `sha256`) earns XP at most once, ever**, across all quests — a
  re-upload or resubmission of the same bytes gets its verdict but `xp_awarded: 0`
  and `xp_reason: "already_rewarded"`.
- XP is an append-only ledger (`xp_ledger`: user, journey, source quest|competition,
  source id, photo, sha256, xp, at). A photographer's journey level is the highest
  `levels[].xp` their journey XP reaches; crossing one answers `level_up: {journey, from, to}`.
- Passing the last quest of a journey (all passed) grants its badge once.
- `quests::on_evaluated` stays the hook for *automatic* reactions to an evaluation;
  it does not auto-submit.

## Timed competitions

```jsonc
{ "title": "Golden hour", "brief": "…", "state": "draft|published|archived",
  "requirements": { … same schema, used as eligibility … },
  "opens_at": …, "closes_at": …,          // entries accepted in [opens_at, closes_at)
  "voting_closes_at": …,                  // votes accepted in [opens_at, voting_closes_at)
  "judging_closes_at": …,                 // judge scores accepted until then; results after
  "weights": { "auto": 0.4, "votes": 0.3, "judges": 0.3 },   // ≥ 0, sum to 1
  "max_entries_per_user": 1,
  "prizes_xp": [300, 200, 100],           // 1st, 2nd, 3rd; credited to the ledger once, at results
  "journey": null,                        // prize XP counts toward this journey's levels, or null = none
  "created_by": "…" }
```

- **Entry**: `POST /api/competitions/{id}/entries {photo_id}` — your evaluated,
  not-hidden photo that passes `requirements`; same refusals as submissions, plus
  `409 competition_closed`, `409 entry_limit`, `409 already_entered` (same photo or
  same `sha256`).
- **Votes**: `PUT /api/competitions/{id}/entries/{entry}/vote {stars: 1..5}` —
  any photographer except the entrant; one per (voter, entry), changeable until
  `voting_closes_at`.
- **Judging**: `PUT /api/competitions/{id}/entries/{entry}/judge {score: 0..10, note?}` —
  curators only, one per (curator, entry), until `judging_closes_at`.
- **Score** (0–100) = `100 × (w.auto·auto + w.votes·votes + w.judges·judges)`, each
  part normalised to 0–1: `votes` = (mean stars − 1)/4, `judges` = mean score/10;
  a part with no inputs yet counts 0 and the leaderboard says so.
- **auto** is `auto-v1` (in `rules.rs`, versioned so a formula change never
  silently re-ranks a finished competition):
  `0.5·clamp(subject_or_focus/8) + 0.3·aesthetics + 0.2·(1 − clip_penalty)`, where
  `subject_or_focus` is the best face `ratio`×10 if a face exists else `focus_ratio`,
  and `clip_penalty = min(1, (clipped_shadows_pct + clipped_highlights_pct)/5)`.
  Missing Vision: aesthetics part is 0 and flagged.
- **Leaderboard** `GET /api/competitions/{id}/leaderboard` — every non-hidden entry
  with its parts and score, highest first; ties broken by earlier entry. Entries
  show the entrant's display name and a thumb URL — this is the **public surface**
  photographers can see (and report from).
- **Results** after `judging_closes_at`: frozen ranking, winners, prize XP credited
  exactly once (idempotent however many times results are read).

## Moderation

- **Report**: `POST /api/photos/{id}/reports {reason: inappropriate|stolen|spam|other, note?}` —
  any photographer, for a photo they can see (their own is `400 own_photo`); one
  open report per (reporter, photo).
- **Admin**:
  `GET /api/admin/reports?state=open|dismissed|actioned`,
  `POST /api/admin/reports/{id}/dismiss {note?}`,
  `POST /api/admin/photos/{id}/hide {reason}` (actions its open reports),
  `POST /api/admin/photos/{id}/unhide`,
  `POST /api/admin/users/{id}/suspend {reason}` / `unsuspend`,
  `POST /api/admin/users/{id}/roles {grant|revoke: "curator"|"admin"}` (an admin
  cannot revoke their own `admin` — `409 last_word`), `GET /api/admin/users`.
- **Hidden photo**: invisible on every shared surface (leaderboards, entries),
  excluded from scoring and from new submissions/entries; its past XP stays (the
  ledger is history) but a hidden competition entry drops out of the ranking. The
  owner still sees it, with `moderation: {hidden: true, reason, at}`.
- **Suspended account**: can log in and see their own photos, cannot upload,
  submit, enter, vote, judge or report (`403 suspended`).
- Every moderation and role action is written to `audit:log` with actor, target and reason.

## Game routes (who → module)

| route | who | module |
|---|---|---|
| `GET /api/journeys`, `GET /api/journeys/{id}` (with my XP, level, quest lock/pass state) | photographer | `progress.rs` |
| `GET /api/quests/{id}`, `POST /api/quests/{id}/submissions`, `GET /api/quests/{id}/submissions` (mine) | photographer | `progress.rs` |
| `GET /api/me/progress` (XP per journey, levels, badges, ledger) | photographer | `progress.rs` |
| `POST/PUT /api/curator/journeys[/{id}]`, `POST /api/curator/journeys/{id}/publish|archive` | curator | `curation.rs` |
| `POST/PUT /api/curator/quests[/{id}]`, `POST /api/curator/quests/{id}/publish|archive` | curator | `curation.rs` |
| `GET /api/curator/journeys`, `GET /api/curator/quests` (all states) | curator | `curation.rs` |
| `GET /api/competitions`, `GET /api/competitions/{id}`, `/leaderboard`, `/results` | photographer | `competitions.rs` |
| `POST /api/competitions/{id}/entries`, `PUT …/entries/{entry}/vote` | photographer | `competitions.rs` |
| `POST/PUT /api/curator/competitions[/{id}]`, `…/publish|archive`, `PUT …/entries/{entry}/judge` | curator | `competitions.rs` |
| `POST /api/photos/{id}/reports` | photographer | `moderation.rs` |
| `/api/admin/*` | admin | `moderation.rs` |

Shared: requirement checking and `auto-v1` live in `rules.rs` (pure functions over a
stored photo record — no I/O, unit-tested); role checks, suspension and "is this
photo hidden" are helpers in `moderation.rs` every module calls.

Time-dependent rules read `now_secs()`. With config `allow-test-routes = true`
(never in production — events-domain's precedent) `POST /test/clock {offset_secs}`
shifts this app's notion of now, so e2e can close a competition without waiting.

## Decided while building (2026-09-24)

The rules above were the design; these are the decisions the code made where the
design was silent. They are as binding as the rest.

**Curation**
- Only the curator who created a journey edits it and its quests (another curator:
  `403 forbidden`, unless also an admin). Every curator can read. Curator writes
  require an active account. Extra reads: `GET /api/curator/journeys/{id}` (with
  `quest_docs`) and `GET /api/curator/quests/{id}`.
- Levels run 1, 2, 3… with strictly ascending XP, first `{1,0}`; default `[{1,0}]`.
  `badge` is optional; null grants none.
- A quest may be published inside a draft journey; photographers see it when the
  journey goes live. Creating a quest appends it to `journey.quests`; `PUT journey
  {quests}` reorders (same set of ids). Publishing an archived journey/quest restores it.
- Requirements are fixed once a quest leaves `draft` (a PUT with identical
  requirements is fine); title, description, xp and window stay editable; a quest
  cannot move journeys. `409 journey_archived` for creating/publishing into an
  archived journey.

**Progress**
- Submit refusal order: quest 404 → photo 404 → `403 forbidden` → `not_evaluated` →
  `photo_hidden` → `quest_locked` → `quest_not_started` → `quest_ended` →
  `quest_not_published` (archived quest in a live journey) → `403 suspended`.
  Windows are `[starts_at, ends_at)`.
- The unlock chain and the badge count only published quests, in `journey.quests` order.
- **"Passed" means a passing verdict, not being paid**: a copied file earns 0 XP but
  still unlocks the next quest. `xp_reason` is `already_rewarded` (these bytes were
  paid before, by anyone) or `already_passed` (this quest was already paid to you).
  The sha256 rule is global and applies to quest XP only; ledger rows exist only for xp > 0.
- `GET /api/me/progress` → `{total_xp, journeys[], badges[], ledger[]}`; ledger rows
  carry `source`, `source_id`, `xp`, `journey`, `photo`, `sha256`, `at`.
- No unique constraints in `records:store`: concurrent duplicate ledger rows or
  badges are settled first-writer-wins (each writer re-reads; the higher ULID deletes itself).

**Competitions**
- An ineligible entry is `422 ineligible` with the verdict. Windows must satisfy
  `opens_at < closes_at ≤ voting_closes_at ≤ judging_closes_at` (`400 bad_windows`);
  `400 bad_journey` for a missing prize journey.
- A published competition changes only `title`/`brief` (`409 competition_published`);
  archived: `409 competition_archived`. Results before judging closes:
  `409 results_pending` with `available_at`.
- Display name on shared surfaces = the local part of the account's email (or
  `photographer-<last 6 of id>`); an account id is never shown.
- Results freeze on first read after judging closes (revision-guarded); each prize
  is claimed before its XP is credited and retried on the next read if crediting
  failed. A photo hidden after results freeze leaves the output; places are not
  renumbered and prizes stand. Hidden entries: votes 404, judging `409 photo_hidden`.
- Known limit: two concurrent entries by one user can exceed `entry_limit` (no
  transactions); duplicate votes/judgements from a race count once, latest wins.

**Moderation**
- Register records `accounts {subject, email}` (auth:identity cannot list accounts);
  the bootstrap admin match is trimmed and case-insensitive.
- A non-owner can see a photo only if it is not hidden and is entered in a
  `published` competition; anything else is `404`, like a missing id, so reports
  cannot probe which ids exist.
- A role grant/revoke applies to the grantee's **existing session on its next
  request** — `auth-guard` re-reads roles from RBAC on every introspect.
- Hidden photo: `urls` are null for everyone but the owner (admins included);
  `moderation.by` is shown to admins only.
- Admin routes are not blocked by suspension; an admin can suspend themself.
- Errors: `409 already_reported`, `409 report_closed`, `400 bad_reason`,
  `400 bad_state`, `400 bad_role`, `400 reason is required` (hide, suspend).
- Suspension storage failure is `503 store_unavailable` — fail closed.

## Game errors (in addition to the table above)

| status | `error` | when |
|---|---|---|
| 403 | `forbidden_role` | route needs curator/admin |
| 403 | `suspended` | account suspended |
| 409 | `not_evaluated`, `photo_hidden`, `quest_locked`, `quest_not_started`, `quest_ended`, `quest_not_published`, `quest_published`, `journey_archived`, `competition_closed`, `competition_published`, `competition_archived`, `results_pending`, `voting_closed`, `judging_closed`, `entry_limit`, `already_entered`, `own_entry`, `already_reported`, `report_closed`, `last_word` | state rules above |
| 422 | `ineligible` + `verdict` | competition entry fails its requirements |
| 503 | `store_unavailable` | suspension check could not read the store (fail closed) |
| 400 | `own_photo`, `bad_requirements` + `detail`, `bad_weights`, `bad_windows`, `bad_levels`, `bad_journey`, `bad_reason`, `bad_state`, `bad_role` | validation |
