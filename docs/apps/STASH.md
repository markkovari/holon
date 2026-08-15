# stash — a note stash you export as a .zip

Keep short notes (title + Markdown); the headline is **"download all my data"** —
`GET /api/export.zip` bundles every note into a real ZIP: one
`notes/<title>.md` per note, an `index.csv`, and a `manifest.json`. The archive
is assembled by the **`zip:archive`** component — there is **no zip library in
the app**; the archive is a link-time capability like everything else here.

Same shape as the other showcases: one **`stash-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts, **`records:store`** for the notes, **`zip:archive`**
for the export, and **`csv:codec`** for the `index.csv` inside it. No bespoke
auth, storage, zip, or CSV. The frontend is a **React + shadcn/ui** SPA (Vite +
Tailwind), served by the host — its bundle carries no archiving code at all.

![The stash app: a notes sidebar with a Markdown editor (title + body, Save/Delete), a New-note button, and an Export .zip button in the header. Clicking Export downloads stash-export.zip; unzipped it holds notes/&lt;title&gt;.md per note plus index.csv and manifest.json. A live recording of the running React app.](docs/media/stash.gif)

## The export (why a `zip:archive` component)

"Export my data" usually drags a zip crate (and a compression backend) into the
app. `stash` inverts that: it builds a list of named byte blobs — a Markdown file
per note, a CSV index (via `csv:codec`), a JSON manifest — and hands them to
`zip:archive::archive`, which returns the finished `.zip` bytes.

`zip:archive` is a small, dependency-free writer using the **STORE** method (no
compression): a local header + raw bytes per file, then a central directory and
the end-of-central-directory record, each value little-endian, each entry with a
CRC-32. It's pure compute — bytes in, bytes out — so the same component bundles a
data export here, a backup elsewhere, or (later) an `.xlsx` (which is a ZIP of
XML). The e2e downloads the archive and parses its central directory; `unzip -t`
verifies the CRCs.

> Store-only keeps it exact and tiny; the archive isn't smaller than its inputs,
> which is fine for bundling already-small text/CSV/JSON. Deflate is a separate,
> much larger job (a rung).

## The data model

- **notes** — `{title, body, created}`, `owner`-scoped. A fresh account is seeded
  a couple of demo notes.

The export walks the owner's notes and produces:

- `notes/<slug>-<id>.md` — `# <title>` + the body (the id suffix keeps names
  unique when two notes share a title).
- `index.csv` — `id,title,created` for every note, formatted by `csv:codec`.
- `manifest.json` — `{app, exported, count, notes:[{id,title}]}`.

## Run it

```bash
just host-stash   # composes the component, builds the React UI, serves on :3046
# register a new account (seeded demo notes), keep notes, and hit Export .zip.
just e2e-stash    # notes CRUD + a valid ZIP export (entry count + manifest)
```

The frontend lives in `examples/stash/ui` (Vite + React + shadcn/ui, **no zip
code**).

## Rungs left

- **Deflate** — add a compressing method to `zip:archive` for larger payloads
  (store is fine for the small text this bundles).
- **`xlsx:codec`** — a spreadsheet is a ZIP of XML parts; `zip:archive` is the
  hard half already done.
- **Import** — round-trip: upload a `stash-export.zip` and restore the notes.
