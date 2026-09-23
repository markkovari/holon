# photoquest — raw photos evaluated on the machine with the GPU

![photoquest: register, upload a 128 MB Sony a7R V ARW straight to the store, and see it come back evaluated on-device — camera data, sharpness, Vision labels, aesthetics. Real time, warm pipeline; the photo is a CC0 sample from raw.pixls.us](../media/photoquest.gif)

A **photo-evaluation app**, and a game built on it. Sign up, drop a 129 MB Sony
ARW onto the page, and a couple of seconds later see it developed, with its
camera metadata, a web-share copy under 10 MB, a per-tile sharpness map, the
sharpness under each face Vision found, labels and an aesthetics score. Then
play with it: submit photos to **quests** that ask for something specific ("grass,
in focus, shot wide open"), earn **XP** and **levels** along **journeys**, finish
a journey for its **badge**, and enter **timed competitions** scored by a mix of
the automatic metrics, other photographers' votes and curators' judging. Curators
build the journeys and competitions; admins moderate. The rules are in the
[CONTRACT](../../components/photoquest-domain/CONTRACT.md) ("The game").

It is here because it is the one showcase whose payload no part of the runtime
can carry. A guest has 64 MiB, every body read stops at 16 MiB, a NATS value is
1 MB. So the bytes never touch a component: the browser uploads them **straight
to S3-compatible storage by presigned multipart**, and a native daemon,
`comp-media`, queues and evaluates them.
[ADR-0098](../adr/0098-photos-live-in-object-storage.md) has the reasoning and
the measurements.

## How it fits together

| piece | what it does |
|---|---|
| `photoquest-domain` | accounts, the `photos` records, who may upload or read what, and verifying the evaluator's signed callback ([CONTRACT](../../components/photoquest-domain/CONTRACT.md)) |
| `media-pipeline` | the `media:pipeline` WIT contract; each call is one loopback HTTP request to `comp-media` ([CONTRACT](../../components/media-pipeline/CONTRACT.md)) |
| `comp-media` (`reconciler/src/bin/media.rs`) | signs upload and rendition URLs, owns the `MEDIA_JOBS` JetStream work queue, and evaluates one photo at a time (`rawler` decode, `green-stab-v1` sharpness). It POSTs the result to `public-callback-base`, signed with `media-callback-secret`, and only to a `--callback-allow` host:port |
| `comp-media-apple` (`tools/media-apple/main.swift`) | macOS only: Core Image develop, Metal sharpness, Vision. Without it the daemon does the same on the CPU with no Vision stage, and the result says which backend ran |
| RustFS (`infra/compose.yaml`, profile `media`) | the object store: `originals` and `renditions` buckets |

## The game

| role | who | does, in the page |
|---|---|---|
| **photographer** | everyone who registers | **Journeys**: each journey with my level, XP towards the next level and its badge; its quests in unlock order, each `open`, `locked` or `passed`, with its XP and deadline. A quest's page says what it asks for in plain words; **Submit a photo** (one of my evaluated photos) answers with a verdict — ✓ / ✗ / — (not looked at: the stage that measures it did not run) per requirement with the measured value, the XP awarded or why none, and a level-up or badge notice. **Competitions**: the phase, brief, windows, scoring weights, prizes and requirements; enter one of my photos (an ineligible one shows its verdict); the leaderboard with each entry's automatic / votes / judges parts; star votes (not on my own); **Report** an entry; after judging, the results and winners. **Progress**: total XP, level per journey, badges, and the XP history. The header shows total XP and my level in each journey I have XP in. A photo an admin hid is marked on my own page with the reason |
| **curator** | granted by an admin | the **Curator** tab: journeys (title, description, the level ladder, badge, quest order ↑↓, publish / archive) and their quests (XP, window, and a requirements form covering the whole schema — subject label or face, focus and face-sharpness ratios, aesthetics, aperture, shutter, focal range, ISO, RAW only, "taken after the start"; fixed once published), and competitions (windows, weights, requirements, prize XP, entries per photographer, the journey prize XP counts toward, publish / archive), with a **Judge** panel: 0–10 and a note per entry |
| **admin** | the bootstrap admin, or granted by an admin | the **Admin** tab: the reports queue by state (open / actioned / dismissed) with the photo's thumbnail — dismiss with a note, hide with a reason (the owner is shown it), unhide; every account with its roles and suspension — grant / revoke curator and admin, suspend with a reason / unsuspend |

Roles add up (a curator is still a photographer; an admin is not implicitly a
curator but can grant themself the role). The Curator and Admin tabs appear only
for those roles, and appear or disappear within a few seconds of a grant or
revoke, without logging in again. The tabs are a convenience: every route checks
the role itself, and a refusal is shown as a message.

**Becoming admin locally.** The first admin is whoever registers with the email in
config `bootstrap-admin-email` (unset: nobody). Give it for a run without editing
the toml, then register that address in the page:

```sh
cargo xtask host photoquest --config bootstrap-admin-email=you@example.com
```

(`apps/photoquest.toml` has the same key commented out as a local-dev example.)
From there the Admin tab grants `curator` — to yourself too — and `admin` to
others. `--config key=value` works for any key and any app: it is passed to
comp-host after the app's `[config]`, so it adds a key or overrides one.

`allow-test-routes=true` enables `POST /test/clock {offset_secs}`, which moves the
game's notion of "now" so a test can pass a deadline without waiting. It is never
in the toml; `e2e/photoquest.sh` passes it with `--config` for its own run.

## Run it locally

```sh
# 1. The store and a JetStream-enabled NATS (the compose nats runs with -js).
docker compose -f infra/compose.yaml --profile media up -d rustfs nats

# 2. The Swift helper, where apps/photoquest.toml's --apple-helper looks for it.
#    Optional: without it, evaluation runs on the CPU.
swiftc -O -o reconciler/target/comp-media-apple tools/media-apple/main.swift

# 3. Compose if needed, start comp-media per [[daemon]], serve on :3941.
cargo xtask host photoquest
```

Then open <http://127.0.0.1:3941>. Browser CORS for the part uploads comes from
comp-media's `--cors-origin` (bucket CORS with `ETag` exposed). Do not also set
`MEDIA_CORS_ORIGINS` on the store: once a bucket has its own rule, that rule
replaces the listener-wide list. The toml's credentials and callback secret are
fixed local-dev values. A real deployment changes the secret on **both** sides
(`[config] media-callback-secret` and `--callback-secret`) and sets `token` and
`media-token` together. When comp-media reaches the store by an internal name,
set `--s3-public-endpoint` to the name browsers use.

comp-media reads its NATS from `--nats-url`, or from **`MEDIA_NATS_URL`** when
the flag is not given (default `nats://127.0.0.1:4222`). `cargo xtask host`
passes its environment to the daemons it spawns, so a JetStream somewhere else
needs no edit to `apps/photoquest.toml`:

```sh
MEDIA_NATS_URL=nats://127.0.0.1:14222 cargo xtask host photoquest
```

That matters on a dev box where :4222 already belongs to some other NATS —
without JetStream, or with another project's streams in it.

`reconciler/tests/gate_photoquest.rs` is the gate. It runs the composed
component against a recording fake `comp-media`, so it needs no store, queue or
GPU: `COMP_HOST=… cargo test --release --test gate_photoquest` in `reconciler/`.

## Scenarios

[`e2e/tests/photoquest.spec.js`](../../e2e/tests/photoquest.spec.js) is the
end-user suite: a real browser against the whole stack (RustFS, a private
JetStream, comp-media with the Swift helper on a Mac, the composed app), with
only CC0 photos. `bash e2e/photoquest.sh` brings all of it up, runs the spec
and tears everything down again; [e2e/README.md](../../e2e/README.md#photoquest)
has the prerequisites.

It is three specs, all run by `photoquest.sh` on one worker (the evaluator is one
queue, and the test clock is one for the whole app): `photoquest.spec.js` (the
photographer), `photoquest-curator.spec.js` and `photoquest-admin.spec.js`.
34 scenarios, about 1.5 minutes warm on an M2 Max. The game's preconditions (a
curator's journeys, another photographer's entry) are set up through the API;
what a scenario is about is done through the page.

What a photographer can do, each one a passing scenario:

- **Account** — register and be signed in; log out and in again; a wrong
  password and an email that already has an account are refused with a message.
- **Upload and evaluate** — an uncompressed and a lossless-compressed a7R V ARW
  and a JPEG are each evaluated; the ARW's detail shows camera, lens, exposure,
  size, the sharpness focus ratio, Vision labels and aesthetics, and which
  backend ran. A JPEG shows what it has without empty or "undefined" rows.
- **Share copy** — the share link is a JPEG under 10 MB, with no EXIF tag
  beyond the structural ones Core Image always writes: no serial, MakerNote,
  make, model, lens, date or GPS.
- **Refusals** — a text file or a PNG is refused, saying which file and what is
  accepted, and leaves no photo behind.
- **Gallery** — newest first, thumbnails load, a click opens the detail.
- **Resilience** — reloading while a photo is still being evaluated picks it up
  again, and it ends `evaluated` without a click.
- **Privacy** — another photographer gets 403 reading or completing my photo and
  never sees it listed; on a shared browser, logging out leaves nothing of mine
  on the page for the next person.

- **Quests** — the active quests with title, what is asked, XP and deadline, an
  ended one shown as ended; a verdict with one line per requirement (✓ grass with
  its confidence, ✗ focus with the measured value and the threshold, — ISO on a
  JPEG that has none); XP awarded once — the same photo again, or the same bytes
  re-uploaded, pass but earn nothing, and the page says why; a photo whose camera
  clock predates the quest is refused, the reason naming both times.
- **Levels** — crossing a threshold shows a level-up and the new level in the
  header, which survives a reload and a fresh login; the XP history lists each
  award with its quest, photo, XP and time, newest first.
- **Journeys** — quests unlock in order; finishing the last grants the badge,
  once.
- **Timed competitions** — an entry is accepted before the deadline and refused
  after it, naming the deadline; the leaderboard ranks by the mixed score (star
  votes cast in the page); after judging, the results with the winners, and the
  prize on the winner's XP history.
- **Moderation effect** — a photo an admin hides leaves the leaderboard for
  everyone else, can no longer be reported, submitted or entered, and its owner
  sees it marked hidden with the admin's reason. Its past XP stays (the
  contract's decision: the ledger is history).

And the curator's and admin's side:

- **Curator** — builds a journey of two quests in the page (levels, badge,
  requirements, reorder, publish), and a photographer sees it in that order and
  plays it through to a level-up and the badge; archiving takes it away; a
  photographer is refused the curator routes, and a bad level ladder is refused
  with its reason. A competition made and published in the page, entered by two
  photographers, judged in the page, ends with the judged favourite winning and
  paid; a late score is refused.
- **Admin** — a photographer reports an entry from the leaderboard; the admin
  sees it in the queue with its thumbnail, hides it with a reason, and it leaves
  the leaderboard while its owner sees the reason; unhiding brings it back. A
  report can be dismissed with a note. Curator granted in the Admin tab shows up
  in the grantee's open page without a re-login, and goes away on revoke. A
  suspended account's upload is refused in the page, and works again once
  unsuspended. An admin cannot revoke their own admin.

**Media in the suite** is only the CC0 samples. The game pays XP once per file
(by sha256) and refuses a second entry of the same bytes, so every upload in the
game scenarios is *salted* — the same CC0 picture with a JPEG comment segment, or
bytes appended after an ARW's end — which gives it its own sha256 and leaves the
evaluation unchanged. The requirements the scenarios use are what the real
pipeline reports for those samples: a `grass` label at ~0.86–0.90, focus ratio
7.2–9.2, no faces, f/1.2, 50 mm, ISO 100, captured 2022-12-17 (so
`captured_after_start: false`, except in the scenario that tests it). On a box
without Vision the label check is dropped.

## Where it stands

Upload, evaluation and the game work end to end on a real box. Two a7R V ARWs (129 MB each), uploaded
through the page on an M2 Max:

- the upload takes about 0.2 s over loopback
- evaluation takes about 2 s, warm, from `complete` to `evaluated`
- the in-focus face scores about 60 and 107; the other face in each frame scores 0.2 and 1.6
- the share copy is 2–2.6 MB

Not built yet: EXIF for JPEG originals (so a JPEG cannot pass exposure or
"taken after the start" requirements — those checks read "not looked at"), and
the original's lifecycle once it has been evaluated (it stays in `originals`).
