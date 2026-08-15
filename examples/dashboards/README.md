# dashboards — server-rendered chart panels (docs/apps/DASHBOARDS.md)

Build dashboards out of **panels** (a title, a chart kind, a data series). The
charts are rendered to **SVG on the server** by the **`svg:chart`** component, so
the frontend ships **no charting library** — it fetches `chart.svg` and drops it
in the page. See [docs/apps/DASHBOARDS.md](../../docs/apps/DASHBOARDS.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui (no charting lib)
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/dashboards.rs      # e2e: seed + a valid SVG per kind + panel round-trip + ownership
```

## Run

```bash
# from the repo root:
just host-dashboards     # composes the component + builds the UI + serves on :3043
```

Open `http://127.0.0.1:3043`: **register** a new account — you get a demo
dashboard. Add panels with the form ("label value" per line, pick a kind); the
server renders the SVG chart. Charts are drawn in `currentColor`, so they follow
the light/dark theme.

```bash
just e2e-dashboards      # seed + a valid SVG per kind (bar/line/donut/sparkline) + ownership
# work on the UI live:
cd examples/dashboards/ui && npm install && npm run dev
```
