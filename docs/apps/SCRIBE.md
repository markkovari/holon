# scribe — a collaborative document editor (convergence, made live)

A **shared document** that many people edit at once with **no lock and no
turn-taking**. Scalar fields (the title) are **CRDT registers**; the **body is
an RGA text sequence** so two people typing in the *same* paragraph
**interleave** instead of one clobbering the other. Every edit is merged
server-side and **streamed to every open editor over SSE**. It's the payoff of
the `crdt` primitive ([CRDT.md](../capabilities/CRDT.md)): where that showcase proved
convergence in the abstract, scribe puts two real browsers on one document and
they converge live.

Same shape as the other showcases: one **`scribe-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — **`crdt:merge`** (the title
is an `lwwmap`, the body an `rga` sequence), **`diff:text`** (per-revision
history — what each edit changed), **`records:store`** (persist it),
**`id:generate`** (replica ids). It's the first app to compose the convergence
class with the realtime-push class (`pulse`'s SSE trick).

![Two editors — Alice and Bob — side by side on one document, each with a History rail. Alice titles the doc and it appears live in Bob's pane; Bob writes the body; then both edit the SAME body field — Bob prepends "DRAFT — " while Alice's appended line stands — and both contributions survive and converge in both panes, something last-writer-wins could not do. Each pane's History rail fills with per-revision unified diffs (green additions, red removals) from the diff:text component. A live two-pane recording of the running app.](../media/scribe.gif)

## Why it's a real CRDT app, not "last save wins"

The naive version of this — a shared doc where the last request to reach the
server overwrites the doc — loses edits and reorders badly under concurrency.
scribe doesn't, because the document is a CRDT and the server *merges* rather
than *replaces*:

| concern | how scribe handles it | the naive version |
|---|---|---|
| two people edit **different fields** at once | both survive — `lwwmap` merges field-wise | second write clobbers the first |
| two people type in the **same body** at once | both survive — the `rga` sequence **interleaves** them deterministically | one overwrites the other |
| the **title** is edited concurrently | deterministic **LWW** on `(ts, replica)` — same result everywhere | depends on request arrival order |
| an edit arrives **late / out of order** | title LWW ignores older; body ops anchor to a stable element id, not a position | a stale write wins because it arrived last |
| two writes **race at the store** | optimistic revision CAS + retry; the retry re-merges (safe because merge is idempotent) | lost update |

Every row is exercised by the e2e (`just e2e-scribe`): different-field survival,
**two inserts at the same body position interleaving to `AYXC`** on every
replica, an id-anchored delete, a **newer title winning over an older one sent
afterward**, and a live SSE connection receiving the merged document.

## How an edit flows

A document is two CRDT states: the scalar fields as an `lwwmap`, the body as an
`rga` sequence.

- **Title** (`{field:"title", value, ts, replica}`): the server does
  `crdt.lwwmap-set`. `ts` is captured at edit time, so a delayed op still
  resolves correctly by last-writer-wins.
- **Body** (`{field:"body", kind:"insert"|"delete", after|ids, text, ts, seq}`):
  the client diffs its textarea into an **id-anchored** op — insert *after an
  element id*, or delete *by id* — never a position. An id is stable under
  concurrency, so a co-editor's insert elsewhere can't shift where yours lands.
  The client mints ids with the same `ts-replica-seq` formula the server uses, so
  it can apply its own op **optimistically** (the next keystroke anchors
  correctly before the round-trip returns) and the server echo is idempotent.

Either way the server stores the new state under an optimistic revision check —
on conflict it reloads and re-merges (idempotent, so the retry converges). `GET
/api/docs/{doc}/events` holds a connection open and pushes the merged document
whenever its revision changes — real server push on wasip2, the same loop as
[pulse](REALTIME.md).

There's no bespoke merge logic in the domain component — the convergence lives
entirely in the composed `crdt:merge` contract. The body is a sequence CRDT and
the title a register purely by which `crdt:merge` calls scribe makes.

## History, for free, by composing `diff:text`

Every edit that actually changes a field's value records a history row; `GET
/api/docs/{doc}/history` returns each revision with a **unified diff** computed
by the `diff:text` component — the right rail in the gif. Two things fall out of
the composition:

- The diff is a real `diff:text` unified diff (`@@` hunks, `+`/`-` lines), the
  same component `track`/`bin` use — scribe writes none of it.
- An edit that **loses** the LWW race (an older timestamp) never changes the
  value, so it leaves **no history row** — the history shows what actually
  happened after convergence, not every request. The e2e asserts the losing
  "Stale rename" never appears.

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

- **Full OT for the focused caret** — the body freezes remote updates while you
  are actively typing in it (so anchors stay correct and text isn't yanked from
  under the caret), resyncing on the next push after blur. A real operational
  transform would apply a co-editor's keystrokes live *without* disturbing your
  caret. Convergence on the server is already always correct; this is a
  client-side nicety.
- **Offline queue** — buffer ops in the client while disconnected and flush on
  reconnect; the timestamps + id-anchored ops already make this correct, it just
  needs the client buffer + a `since` catch-up.
