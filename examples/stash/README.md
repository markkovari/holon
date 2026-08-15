# stash — a note stash you export as a .zip (docs/apps/STASH.md)

Keep short Markdown notes; **`GET /api/export.zip`** bundles them all into a real
ZIP — one `notes/<title>.md` per note, an `index.csv` (via `csv:codec`), and a
`manifest.json` — assembled by the **`zip:archive`** component. No zip library in
the app. See [docs/apps/STASH.md](../../STASH.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui (no archiving code)
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/stash.rs           # e2e: notes CRUD + a valid ZIP export (parses the central directory)
```

## Run

```bash
# from the repo root:
just host-stash          # composes the component + builds the UI + serves on :3046
```

Open `http://127.0.0.1:3046`: **register** a new account — you get a couple of
demo notes. Edit notes, then hit **Export .zip** to download them all; `unzip` it
to see `notes/*.md`, `index.csv`, and `manifest.json`.

```bash
just e2e-stash           # notes CRUD + a valid ZIP export
# work on the UI live:
cd examples/stash/ui && npm install && npm run dev
```
