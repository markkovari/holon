# tempo — a multi-person worktime logger (with charts)

Everyone logs time against a **project** and a **work category** (engineering,
sales, design, …); admins create the projects and categories. Log manually or
run a live **pomodoro timer** (start → stop → an entry with the elapsed minutes),
and edit/delete your own entries. Anyone sees their own contribution over a
**week / month / year / custom range**, broken down by project and category; a
**project lead** sees that project's whole distribution, including who
contributed what. The reporting axis — group + sum over a date range, scoped by
role — is what the charts render.

Same shape as the other showcases: one **`tempo-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts + RBAC, **`records:store`** for the data, and
**`pdf:codec`** to export a range report as a downloadable PDF. No bespoke auth,
no bespoke storage, no PDF library. The frontend is a **React + shadcn/ui** SPA
(Vite + Tailwind, charts by **recharts**), mobile-friendly, built to
`examples/tempo/dist` and served by the host — no framework in the backend.

![The tempo dashboard on a phone: a project lead signs in, the Reports tab shows the team's month — a donut of hours by project, bars by category / per-day / per-person (recharts) — flips the range and the Everyone/Mine scope, and exports the range as a PDF; the Calendar tab shows today's time-grid with scheduled blocks, a tap on an empty slot adds an entry, and tapping a block opens an editor to change or delete it; the Log tab runs a pomodoro. A live recording of the running React app at a mobile viewport.](docs/media/tempo.gif)

The SPA has three surfaces: **Log** (quick entry + pomodoro + recent list), a
**Calendar** day view (a Google-calendar-style time-grid — see a day's entries as
positioned blocks, tap an empty slot to add one at that time, tap a block to edit
or delete it), and **Reports** (the charts + a **PDF** export of the current
range). Tapping any entry — a calendar block, an unscheduled chip, or a Log-tab
row — opens the same editor. Admins get an **Admin** tab (projects, categories,
membership).

## The capability model

**Two global roles** (self-assigned at register in the demo; an admin would grant
them in prod) plus **per-project membership** — a user's reach is defined by the
projects they belong to, not a global "manager" flag.

| capability | member | member (as a project **lead**) | admin |
|---|:--:|:--:|:--:|
| log time / pomodoro **on a project they belong to** | ✓ | ✓ | ✓ (any project) |
| log on a project they're **not** in | ✗ | ✗ | ✓ |
| view / edit / delete **their own** entries | ✓ | ✓ | ✓ (any) |
| see a **project's** whole distribution (by person) | ✗ | ✓ *(their led projects)* | ✓ (all) |
| create projects / categories | ✗ | ✗ | ✓ |
| add members to a project (member / lead) | ✗ | ✓ *(their led projects)* | ✓ |

Every write checks the caller's token (`authorizer::introspect`). Logging is
gated by membership (`can_log`); the report's `scope=all` is gated by
`can_see_all` (admin, or a lead of ≥1 project) and only ever returns the led
projects' entries — a plain member asking for `all` is silently kept to `me`.

## The data model

- **projects** / **categories** — admin-created named records.
- **memberships** — `{project, user, role: member|lead}`; the ACL that decides
  who can log where and who sees a project's team view.
- **entries** — `{user, project, category, minutes, day, note}`. The `day` is a
  `YYYY-MM-DD` string, so a range filter is a **string compare** (`from <= day <=
  to`) and the client owns the calendar — no server-side date math. Project and
  category names are denormalized onto the entry for fast reporting.
- **timers** — one running "pomodoro" per user; `stop` computes elapsed minutes
  and writes an entry, then deletes the timer.

## The report (what the charts read)

`GET /api/report?from&to&scope=me|all` sums minutes over the range, grouped every
way the dashboard needs, in one call:

- `by_project` — the donut + total.
- `by_category` — category bars.
- `by_day` — the per-day series.
- `matrix` — project × category (for a stacked view).
- `by_user` — per-person bars (leads/admins only).

`GET /api/report.pdf?from&to&scope` runs the *same* aggregation and hands the
totals to **`pdf:codec`**, which lays them out as a paginated PDF 1.4 file
(built-in Helvetica, WinAnsi text, exact `xref` — no font embedding, no headless
browser). The **Reports** tab's **PDF** button downloads it. `pdf:codec` is pure
compute (`render: document -> list<u8>`), so any showcase can reuse it for
receipts or summaries.

The whole thing is exercised by `just e2e-tempo`: admin-only project/category
creation, membership-gated logging (a non-member is `403`), owner edit/delete,
range filtering, a **project lead** seeing that project grouped by user (a plain
member can't widen scope), and the pomodoro timer producing an entry.

## Run it

```bash
just host-tempo     # builds the React UI + runs the native host + SPA on :3040
# register as admin to create projects/categories + assign membership;
# as member to log; a project lead gets the team view.
just e2e-tempo      # the auth + membership + aggregation + timer e2e
```

The frontend lives in `examples/tempo/ui` (Vite + React + shadcn/ui + recharts);
`just host-tempo` builds it to `examples/tempo/dist`, which the host serves.

## Deploy — the simple way (one process / one container)

You don't need wasmCloud to run this. The repo's **`comp-host`** is a single
binary that serves `wasi:http`, the SPA (`--static-dir`), and `wasi:keyvalue`
in-process — with a built-in **Redis** backend. The whole app is one command:

```bash
comp-host --component tempo.composed.wasm --addr 0.0.0.0:8080 \
  --kv redis --redis-url rediss://default:PW@host:25061 --static-dir dist
```

Package that as **one image** (`just docker-tempo` → `examples/tempo/Dockerfile`)
and run it anywhere:

```bash
docker run -p 8080:8080 -e REDIS_URL='rediss://default:PW@host:25061' tempo
```

The container serves the API *and* the SPA on the same origin (no CORS, no proxy)
and talks to a managed Redis/Valkey over TLS — the only moving parts are the
image and the database. See `examples/tempo/Dockerfile` for the DigitalOcean
droplet / App Platform recipe.

## Publish the component (for the wasmCloud path)

The composed `tempo` component (`components/target/tempo_domain.composed.wasm` —
tempo + auth-guard + record-store) is self-contained; its only runtime imports
are `wasi:keyvalue / http / config / clocks / random`, bound at deploy time. So
storage (Redis, NATS, in-memory, …) is a **link choice**, not code. If you'd
rather run it on a wasmCloud lattice (scale, multi-tenant, live linking):

Publish it to GHCR as a **public** OCI artifact (the wasmCloud-native pull path):

```bash
gh auth refresh -s write:packages     # once
just push-tempo-ghcr 0.1.0            # gh mints the token, wash does the OCI push
# make the package Public once (GitHub → Packages → tempo → visibility)
```

or let CI do it on a `tempo-v*` tag ([`.github/workflows/tempo-ghcr.yaml`](../.github/workflows/tempo-ghcr.yaml)).
Then any wasmCloud host pulls it anonymously and links storage to, e.g., a
Redis/Valkey provider:

```bash
wash start component oci://ghcr.io/<owner>/tempo:0.1.0 tempo
wash start provider ghcr.io/wasmcloud/keyvalue-redis:0.28.0 kv
wash config put redis URL=rediss://default:<pw>@<valkey-host>:25061
wash link put tempo kv wasi keyvalue --interface store --interface atomics --interface batch --target-config redis
```

DOCR can't host it publicly (private-only); GHCR public matches a public repo and
needs no pull credentials on the host.

## Rungs left

- **Admin-managed roles** — drop self-assign at register; an admin promotes to
  the global `admin` role (membership is already admin/lead-managed).
- **Calendar grid + weekly submit** — a month grid view (the data carries `day`).
- **CSV export** — the range as CSV via the `csv:codec` component (PDF already ships).
