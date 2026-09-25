# e2e — six authored manifests, one fleet, real requests

Two suites live here: the Rust **manifest** suite below (`fixtures/`, driven by
`reconciler/tests/e2e.rs`), and a **Playwright** browser suite for ten showcase apps
(`tests/`, [further down](#the-playwright-suite)), plus
[photoquest](#photoquest), which brings up its own object store, queue and
evaluator, and [vcs](#vcs), the code store against real NATS and SurrealDB.

```
cargo build --release --manifest-path host/Cargo.toml
cargo nextest run --release --manifest-path reconciler/Cargo.toml -E 'test(six_manifests)'
```

Needs `nats-server` on PATH, the built `comp-host`, and the component artifacts
(`cargo xtask build --force`, `cargo xtask compose gate`). Runs in about eight seconds.

The fixtures in `fixtures/` are **authored** documents — what a person writes — and the test
converts them through `spec::AppSpec::to_manifest`, the same code path a real deploy
uses. Nothing in `fixtures/` contains a digest, a `host_needs` list nobody chose,
or an `egress` policy; those are stamped by the platform.

| fixture | must |
|---|---|
| `fixtures/fused.yaml` | serve — a composed artifact over HTTP |
| `fixtures/linked.yaml` | serve — `gate`'s two imports bound to `record-store` and `shaper` at runtime |
| `fixtures/zero.yaml` | serve — `min: 0`, activated by the request itself (ADR-0042) |
| `fixtures/conflict.yaml` | be refused — two providers of one interface |
| `fixtures/ungrantable.yaml` | be refused — a capability no host grants |
| `fixtures/unplaceable.yaml` | be refused — a constraint no node advertises |

## Why it is shaped like this

**Positives are checked by invoking them.** An app that is placed but does not answer
is exactly the failure a status check misses — inventory would show it running.

**Negatives are checked by their reason, not just by failing.** Asserting "it was
refused" passes for a refusal with the wrong reason, and a reason nobody can act on is
barely better than a crash. `conflict` must name the interface *and both providers*;
`unplaceable` must name the constraint it could not meet.

**They deploy together on purpose.** Three broken manifests alongside three healthy
ones is the assertion that a bad manifest cannot stop good apps from being placed — a
property no single-app test can show, and the kind of thing that breaks when the
planner grows a new early return.

**It polls rather than sleeping on a number.** Inventory is a heartbeat behind reality
and a parked app has to be activated first; a test that asserts on a snapshot taken at
the wrong moment fails on a working system. That mistake has been made twice in this
repo (ADR-0042, ADR-0045) and costs more than the polling does.

**One language, one runner.** The control plane is an axum stub inside the test rather
than a script, so the fixtures, the conversion, the assertions and the harness are all
Rust under `cargo nextest`. The previous bash+python version took about a minute; this
takes eight seconds, because the stub is in-process and nothing sleeps on a guess.

## Adding a fixture

Write the YAML in `fixtures/`, then add the app to the serve list or the refusal table in
`reconciler/tests/e2e.rs`. Refusal entries carry the substrings the reason must
contain — write the ones an operator would need in order to fix it, not the ones that
happen to be in the string today.

## The Playwright suite

```
cd e2e && npm install && npx playwright install chromium
cargo build --release --manifest-path reconciler/Cargo.toml --bin comp-plug   # from the repo root
cd e2e && bash run_all.sh
```

`run_all.sh` runs one app at a time, for the ten `*-domain` apps whose specs are in
`tests/` (`book-lending`, `ecommerce-fulfillment`, `moderation`, `photosocial`,
`pipeline`, `real-estate-escrow`, `smart-home`, `support`, `ticket-triage`,
`volunteer-shift`): it builds the app's components, composes them with `comp-plug`,
starts `comp-host` on `127.0.0.1:3000` over `--kv sqlite`, runs
`tests/<app>.spec.js` against the app's own browser UI, and stops the host. It stops
at the first app that fails. Run it from inside `e2e/`, because its paths are
relative to that directory. `playwright.config.js` records a video of every test.

To run one app's spec against a host you started yourself: `npx playwright test
tests/<app>.spec.js`.

## photoquest

```
bash e2e/photoquest.sh                 # from anywhere; extra args go to playwright
bash e2e/photoquest.sh -g Privacy      # e.g. one group
```

Three specs drive the photoquest page against the whole stack, not a fake:
RustFS, JetStream, `comp-media` and the composed app
([docs/apps/PHOTOQUEST.md](../docs/apps/PHOTOQUEST.md)).
[`tests/photoquest.spec.js`](tests/photoquest.spec.js) as a **photographer**
(upload and evaluation, then the game: quests, levels, journeys, timed
competitions, the effect of moderation),
[`tests/photoquest-curator.spec.js`](tests/photoquest-curator.spec.js) as a
**curator** (a journey and a competition built, published, judged in the page)
and [`tests/photoquest-admin.spec.js`](tests/photoquest-admin.spec.js) as an
**admin** (reports, roles, suspension). `photoquest.sh` owns all of it:

1. downloads the two CC0 Sony a7R V samples from raw.pixls.us into
   `e2e/.photoquest-samples/` (gitignored, ~210 MB, checked by sha256, once), and
   cuts a JPEG sample out of one of them — its embedded preview — so every file the
   suite uploads is CC0. Nothing personal, nothing committed.
2. starts RustFS (`infra/compose.yaml`, profile `media`) as its own compose
   project `holon-photoquest-e2e`, with its own volume;
3. starts a private `nats-server -js` on a free port with a temp store, and points
   comp-media at it with `MEDIA_NATS_URL` — a dev box's :4222 is often someone
   else's NATS;
4. on macOS, builds the Swift helper `comp-media-apple` if it is missing or stale;
5. runs `cargo xtask compose photoquest`, then `cargo xtask host photoquest` (which
   builds and starts comp-media per `[[daemon]]`) with the host's sqlite in a temp
   dir, waits for `:3941/health` and comp-media's `/health` (store and queue up);
6. gives the app two config keys for this run only, with `cargo xtask host
   --config key=value` (added after the toml's `[config]`), and checks the first
   one arrived: `allow-test-routes=true` for `POST /test/clock` (the scenarios
   pass deadlines by moving the app's clock, and put it back after each test) and
   `bootstrap-admin-email=admin@photoquest.test` (`PHOTOQUEST_ADMIN_EMAIL`
   overrides) — the suite's admin, who grants every curator. Neither is in the
   committed `apps/photoquest.toml`: production must not have test routes;
7. warms the pipeline with one upload (Core Image's first job is ~4 s, later ~2 s);
8. runs the three specs with `--workers=1` — the evaluator is one queue, the
   resilience scenario counts on what is ahead of it there, and the test clock is
   one for the whole app — and on exit — pass, fail or Ctrl-C — kills the host
   and the daemon (their whole process group), the NATS, and removes the
   containers, the volume and the temp dirs.

**Prerequisites:** Docker running; `nats-server`, `node`/`npx`, `cargo`, `curl` on
PATH; on macOS the Xcode command-line tools (`swiftc`). `npm ci` and `npx playwright
install chromium` are run for you. Ports 3941, 8013 (fixed by
`apps/photoquest.toml`) and 9000–9001 (the store) must be free; the script says so
if they are not. A warm run is 34 scenarios in about 1.5 minutes (under two
minutes for the whole script); from a clean checkout the release builds of
comp-host and comp-media dominate.

**Game media.** Every photo is still one of the CC0 samples, but the game pays XP
once per file (sha256), ever, and refuses a second entry of the same bytes, so
`lib/photoquest.js` uploads them *salted* (`upload(..., { salt: true })`): a
JPEG comment segment, or bytes after an ARW's end — a sha256 of its own, the same
evaluation. Requirements match what the pipeline really reports for the samples
(`GRASS`, `passable()` in `lib/photoquest.js`): a `grass` label, focus ratio
≥ 5, and `captured_after_start: false` because they were taken in 2022 — except
in the scenario about exactly that.

**Linux / CI:** no Swift helper, so comp-media evaluates on the CPU (`rawler`
develop, CPU sharpness, no Vision). The specs ask comp-media's `/health` which
backend is present and, without it, expect `cpu sharpness, rawler develop, no
Vision` and no labels or aesthetics rows instead, and leave the `grass` label out
of the game's requirements (a label check without Vision is "not looked at",
which never passes); everything else is asserted the same. It needs the same
Docker, `nats-server` and ports.

**Against a stack you started yourself** (per docs/apps/PHOTOQUEST.md), started
with the same two keys — `cargo xtask host photoquest --config
allow-test-routes=true --config bootstrap-admin-email=admin@photoquest.test` — on
a fresh store: `PHOTOQUEST_SAMPLES=<dir with the samples> npx playwright test
tests/photoquest*.spec.js --workers=1`. `PHOTOQUEST_URL` and
`PHOTOQUEST_MEDIA_URL` override `http://127.0.0.1:3941` and `:8013`. The API
helpers the specs use for setup (users, a curator, the admin, journeys,
competitions, the clock, uploads) are in `lib/photoquest.js`; the page helpers
the three specs share (sign in, open a journey or a competition, submit a photo)
in `lib/photoquest-page.js`.

## vcs

```
bash e2e/vcs.sh                 # every scenario once
RUNS=5 bash e2e/vcs.sh          # five times over one set of services
bash e2e/vcs.sh c_crash         # a test-name filter
```

The code store (ADR-0099) end to end: `reconciler/tests/e2e_vcs.rs` drives
`comp-host` (vcs-gateway ⊕ vcs-store) → `comp-vcs` → NATS JetStream + SurrealDB
over HTTP only — two agents commuting and conflicting, a real `abort()` of
`comp-vcs` at each step of the write order and a restart, the rename race,
record-store's real sources ingested and materialized with git tree ids checked
against `git`, a move, and the lease. The script starts the compose SurrealDB
(`--profile graph`, its own compose project) unless `VCS_E2E_SURREAL_URL` is set,
and a private `nats-server -js` on a free port; builds the two components and
`comp-host`; and on exit removes all of it. Scenarios and timings:
[docs/apps/VCS.md](../docs/apps/VCS.md#the-e2e-suite). Needs Docker (or a
SurrealDB), `nats-server`, `git`, `cargo`; port 8000 free.
