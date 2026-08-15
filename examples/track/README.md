# track — a Linear-lite project tracker (e2e)

The [docs/apps/TRACK.md](../../TRACK.md) showcase — the *complex* one — as one composed
wasm HTTP component on the native Rust host, with a **Vite + TypeScript SPA baked
into the wasm** (no `--static-dir`). Five axes over ~15 contracts: auth+RBAC,
a board state machine, full-text search, a live SSE activity feed, a background
stale-sweep, signed outbound webhooks, and AI thread-summary.

## Layout

```
examples/track/
  ui/            the Vite + TS SPA (src/api.ts, src/main.ts, src/style.css)
  tests/         the e2e (drives all five axes)
```

The SPA builds into `components/track-assets/static/`, which the `track-assets`
component embeds at build time — so the composed wasm serves its own UI.

## Run it

```bash
just host-track       # from repo root; tracker on http://127.0.0.1:3025
```

`build-track-ui` (npm + Vite) runs automatically before the compose. Open the
board: **register** (the first user is admin), **create a project**, **file
issues**, **move** them across backlog → todo → in progress → done, **comment**
(markdown), hit **✨ summarize** for an AI thread summary, **search**, and watch
the **activity feed** stream every change live over SSE. The **sweep** button
runs the background stale-issue tick.

Open the board in **two tabs** (or watch `docs/media/track-sse.gif`): file or
move an issue in one, and the other's activity feed + board update **live** over
SSE — the `event:bus` fan-out across independent clients, no WebSocket.

Swap the mock LLM for a real one: compose with `openai-provider` instead of the
mock (`compose-ai-openai`) and set the `openai:*` config — `track-domain` is
unchanged.

## Test it

```bash
just e2e-track        # builds the SPA + composes + builds host + runs tests/track.rs
```

One test drives **all five axes**: auth + RBAC (a non-admin can't create a
project; a non-member is 403 on write), the issue lifecycle over the fsm (with a
409 on an illegal move), full-text search, a live SSE `issue.created` frame, the
background sweep tick, and the AI thread summary.

## What's composed (the widest in the repo — 13 plugs)

`track-domain` (`track:app`) imports only contracts:

- `auth:identity` (the pre-composed auth-guard) — accounts / authorizer /
  session / rbac
- `policy:guard` — per-project membership ABAC
- `records:store` · `fsm:workflow` · `search:index` · `paginate:cursor`
- `event:bus` — the SSE + webhook spine
- `notify:dispatch` + `webhook:sign` — signed outbound webhooks
- `ai:inference` (+ the mock `llm:inference`) — thread summary
- `md:render` — comment/body HTML
- `ui:assets` (the `track-assets` component) — the baked SPA

plus host WASI (keyvalue, clocks, config, http). Auth-guard and ai-inference are
each pre-composed, then plugged into the domain.
