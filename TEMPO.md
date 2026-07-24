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
(`auth:identity`) for accounts + RBAC, and **`records:store`** for the data. No
bespoke auth, no bespoke storage. The frontend is a **React + shadcn/ui** SPA
(Vite + Tailwind, charts by **recharts**), mobile-friendly, built to
`examples/tempo/dist` and served by the host — no framework in the backend.

![The tempo dashboard on a phone: a project lead signs in, the Reports tab shows the team's month — a donut of hours by project, bars by category / per-day / per-person (recharts) — flips the range and the Everyone/Mine scope; the Calendar tab shows today's time-grid with scheduled blocks and a tap on an empty slot adds an entry right there; the Log tab runs a pomodoro. A live recording of the running React app at a mobile viewport.](docs/media/tempo.gif)

The SPA has three surfaces: **Log** (quick entry + pomodoro + recent list with
edit/delete), a **Calendar** day view (a Google-calendar-style time-grid — see a
day's entries as positioned blocks and tap an empty slot to add one at that
time), and **Reports** (the charts). Admins get an **Admin** tab (projects,
categories, membership).

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

## Rungs left

- **Admin-managed roles** — drop self-assign at register; an admin promotes to
  the global `admin` role (membership is already admin/lead-managed).
- **Calendar grid + weekly submit** — a month grid view (the data carries `day`).
- **Export** — CSV of a range via the `csv:codec` component.
