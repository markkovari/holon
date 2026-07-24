# scribe — a collaborative document editor (convergence, made live)

A **shared document** that many people edit at once with **no lock and no
turn-taking**. Each field is a **CRDT register**; an edit from any window is
merged server-side and **streamed to every open editor over SSE**. It's the
payoff of the `crdt` primitive ([CRDT.md](CRDT.md)): where that showcase proved
convergence in the abstract, scribe puts two real browsers on one document and
they converge live.

Same shape as the other showcases: one **`scribe-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — **`crdt:merge`** (the
document *is* an `lwwmap` CRDT state), **`records:store`** (persist it),
**`id:generate`** (replica ids). It's the first app to compose the convergence
class with the realtime-push class (`pulse`'s SSE trick).

![Two editors — Alice and Bob — side by side on one document. Alice types the title "Launch plan" and it appears live in Bob's pane; Bob writes the body and it appears live in Alice's; then Alice revises the title to "— v2" while Bob appends a line to the body — different fields edited from different replicas, both survive the merge, and both panes show the identical merged document. A live two-pane recording of the running app.](docs/media/scribe.gif)

## Why it's a real CRDT app, not "last save wins"

The naive version of this — a shared doc where the last request to reach the
server overwrites the doc — loses edits and reorders badly under concurrency.
scribe doesn't, because the document is a CRDT and the server *merges* rather
than *replaces*:

| concern | how scribe handles it | the naive version |
|---|---|---|
| two people edit **different fields** at once | both survive — `lwwmap` merges field-wise | second write clobbers the first |
| two people edit the **same field** | deterministic **LWW** on `(ts, replica)` — same result everywhere | depends on request arrival order |
| an edit arrives **late / out of order** | ignored if older by timestamp — never clobbers a newer value | a stale write wins because it arrived last |
| two writes **race at the store** | optimistic revision CAS + retry; the retry re-merges (safe because merge is idempotent) | lost update |

Every row is exercised by the e2e (`just e2e-scribe`): different-field survival,
a **newer edit winning over an older one that was sent afterward**, and a live
SSE connection receiving the merged document.

## How an edit flows

1. A window sends `POST /api/docs/{doc}/ops {field, value, ts, replica}`. `ts` is
   captured **at edit time** (so a delayed op still resolves correctly), and
   `replica` identifies the editor.
2. The server merges it: `crdt.lwwmap-set(state, field, value, ts, replica)`,
   then stores the new state under an optimistic revision check — on conflict it
   reloads and re-merges (idempotent, so the retry converges).
3. `GET /api/docs/{doc}/events` holds a connection open and pushes the merged
   document whenever its revision changes — real server push on wasip2, the same
   loop as [pulse](REALTIME.md). Every open window applies the merge, skipping
   any field the user is actively typing in (so it never stomps a live caret).

There's no bespoke merge logic in the domain component — the convergence lives
entirely in the composed `crdt:merge` contract. Swap the CRDT type and the
conflict semantics change without touching scribe.

## Run it

```bash
just host-scribe          # native host + SPA on http://127.0.0.1:3037
# open two browser windows on that URL (or add ?doc=notes&name=Alice) and type
just e2e-scribe           # the convergence + live-SSE e2e
```

Regenerate the gif (`tools/screencast/`): `just host-scribe &`, then `node
tools/screencast/scribe.mjs` and `bash to-gif.sh videos/scribe/*.webm
../../docs/media/scribe.gif 800 10`.

## Rungs left

- **Rich text via a sequence CRDT** — fields are last-writer-wins registers, so
  two people typing into the *same* field resolve by LWW (one wins) rather than
  interleaving characters. An RGA / text-sequence CRDT would merge concurrent
  character inserts; the `lwwmap` per field is rung 1.
- **Per-edit history / diff** — compose `diff:text` to show what each revision
  changed (blocked only on that PR landing).
- **Offline queue** — buffer ops in the client while disconnected and flush on
  reconnect; the timestamps already make this correct, it just needs the client
  buffer + a `since` catch-up.
