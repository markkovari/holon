# dashboards — metric panels, charts rendered on the server

Build a dashboard out of **panels** — a title, a chart **kind** (bar / line /
donut / sparkline) and a little data series. The point: the charts are rendered
to **SVG on the server** by the **`svg:chart`** component, so the frontend ships
**no charting library**. The same SVG works in the page, an email, or a report —
"data in, chart out", bound at composition time like every other capability here.

Same shape as the other showcases: one **`dashboards-domain`** HTTP component
that exports `wasi:http` and imports only WIT contracts — the composed
**auth-guard** (`auth:identity`) for accounts, **`records:store`** for the data,
and **`svg:chart`** to render each panel. No bespoke auth, storage, or chart
renderer. The frontend is a **React + shadcn/ui** SPA (Vite + Tailwind),
mobile-friendly, served by the host — and its production bundle is **~60% smaller
than [tempo](TEMPO.md)'s**, precisely because there is no `recharts` (or any
charting code) in it.

![The dashboards app on a phone: a new account signs in to a seeded demo dashboard whose panels are a bar chart (hours by project, with value labels), a donut (effort split, with a legend and a centre total), a line (this week) and a sparkline (signups) — all rendered as SVG by the server; then an Add-panel form takes a title, a kind, and “label value” lines and a new donut appears. A live recording of the running React app at a mobile viewport.](../media/dashboards.gif)

## The idea (why a chart *component*)

Reporting UIs usually pull a JS charting library (or a headless browser) into the
frontend. `dashboards` inverts that: the panel's series is `POST`ed as data, and
`GET /api/panels/{id}/chart.svg` returns a finished `<svg>` that `svg:chart`
built. Because a chart is just a string:

- the **frontend has no charting dependency** — it fetches the SVG and drops it
  in the page;
- axes and labels are drawn in **`currentColor`**, so a chart follows the
  surrounding light/dark theme;
- the **same renderer** is reusable everywhere a chart is needed — an email body,
  a status badge, or (rasterized) a PDF report.

`svg:chart` is a standalone, dependency-free component (`render: chart -> string`)
with four kinds: **bar**, **line**, **donut**, **sparkline** — one series each,
the common report shape.

## The data model

- **dashboards** — `{name, owner}`; each account owns its own (a fresh account is
  seeded a **demo dashboard** so the app is never empty).
- **panels** — `{dashboard, title, kind, data}` where `data` is a list of
  `{label, value, color?}`. That single shape drives every chart kind.

Every read is ownership-checked against the caller's token
(`authorizer::introspect`); one account cannot see or render another's panels.

## Run it

```bash
just host-dashboards   # composes the component, builds the React UI, serves on :3043
# register a new account (seeded with a demo dashboard), then add panels —
# "label value" per line, pick a kind, and the server renders the SVG.
just e2e-dashboards    # seed + a valid SVG per kind + panel round-trip + ownership
```

The frontend lives in `examples/dashboards/ui` (Vite + React + shadcn/ui, **no
charting library**); `just host-dashboards` builds it to
`examples/dashboards/dist`, which the host serves.

## Rungs left

- **Aggregating panels** — a panel that sums/​groups from a `records` collection
  instead of literal data (turn any dataset into a chart).
- **Charts in the PDF export** — teach `pdf:codec` to embed an SVG (or a
  rasterized chart) so [tempo](TEMPO.md)'s PDF report carries the real charts
  `svg:chart` already renders.
- **Multi-series** — grouped bars / stacked / multi-line (svg:chart is
  single-series today).
- **Sharing** — a read-only public link to a dashboard.
