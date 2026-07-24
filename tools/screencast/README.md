# screencast — Playwright → GIF harness for the showcase demos

Records the showcase apps and produces the optimized GIFs in
[`../../docs/media/`](../../docs/media) that the app docs embed.

## Setup (once)

```bash
npm install                 # playwright
npx playwright install chromium
# ffmpeg + gifsicle for the webm → optimized gif step:
brew install ffmpeg gifsicle
```

## Regenerate a gif

Each recorder launches the app, drives a browser (headless), records a `.webm`,
and `to-gif.sh` converts it to a palette-optimized, gifsicle-shrunk gif.

**eShop** — needs the storefront running:

```bash
docker compose -f ../../infra/compose.yaml up -d nats
just host-eshop &                       # from repo root; gateway on :3100
node eshop.mjs
bash to-gif.sh videos/eshop/*.webm ../../docs/media/eshop.gif 800 10
```

**vet-clinic (petclinic)** — native host + seed (the jco boot seeds in-process;
the native host doesn't, so `seed-vet.sh` bootstraps roles + demo users over the
unguarded `/admin` routes):

```bash
# from repo root: host/target/release/vet-host --component \
#   components/target/vet_domain.full.composed.wasm --addr 127.0.0.1:3007 \
#   --static-dir examples/jco-vet-clinic/public &
bash seed-vet.sh
node vet.mjs
bash to-gif.sh videos/vet/*.webm ../../docs/media/petclinic.gif 800 10
```

**conduit** — API-only, so we film its real proof: `conformance-term.html`
streams the captured `just conformance-conduit` output (13/13 green) as a
terminal animation.

```bash
node conduit-term.mjs
bash to-gif.sh videos/conduit/*.webm ../../docs/media/conduit-conformance.gif 860 12
```

**pulse** — realtime chat; two panes side by side, a message in one appears live
in the other over SSE:

```bash
just host-pulse &                       # from repo root; board on :3015
node pulse.mjs
bash to-gif.sh videos/pulse/*.webm ../../docs/media/pulse.gif 800 12
```

**pipeline** — reliable delivery board; a burst marches Pending → In-flight →
Done, the sink is taken down so events retry into the dead-letter tray, then a
Replay redelivers — the whole retry/backoff/DLQ story live over SSE:

```bash
just host-pipeline &                    # from repo root; board on :3016
node pipeline.mjs
bash to-gif.sh videos/pipeline/*.webm ../../docs/media/pipeline.gif 820 12
```

**flags** — rollout console; add a flag, drag it to 30% then 60% (sticky,
monotone cohorts light up), then trip the kill-switch (all dark):

```bash
just host-flags &                       # from repo root; console on :3017
node flags.mjs
bash to-gif.sh videos/flags/*.webm ../../docs/media/flags.gif 820 12
```

**abtest** — A/B/n experiment console; two users in different arms, shift a
weight (sticky re-bucket), convert tiles and watch per-arm rate bars pull apart:

```bash
just host-abtest &                      # from repo root; console on :3018
node abtest.mjs
bash to-gif.sh videos/abtest/*.webm ../../docs/media/experiment.gif 840 12
```

**search** — search-as-you-type; type to narrow ranked hits, toggle all-mode,
filter by facet, repeat a query to see the ⚡ cached badge + hit-ratio climb:

```bash
just host-search &                      # from repo root; console on :3019
node search.mjs
bash to-gif.sh videos/search/*.webm ../../docs/media/search.gif 720 12
```

**ratelimit** — throttle wall; burst to drive the attempt bar to its ceiling,
watch the key LOCK with a countdown + the quota gauge drain, then Reset:

```bash
just host-ratelimit &                   # from repo root; wall on :3020
node ratelimit.mjs
bash to-gif.sh videos/ratelimit/*.webm ../../docs/media/ratelimit.gif 760 12
```

**crdt** — no UI/server (pure compute), so this recorder computes real
`crdt.wasm` output in Node and lays three replicas across three panes: they edit
offline and diverge, then a SYNC merges them and all converge to the identical
state.

```bash
cd ../../examples/jco-crdt && npm run transpile   # produce gen/ (once)
cd -                                              # back to tools/screencast
node crdt.mjs
bash to-gif.sh videos/crdt/*.webm ../../docs/media/crdt.gif 900 12
```

**scribe** — collaborative editor; two panes on the REAL running app editing one
document. Each pane is a distinct replica (`?rid=`); an edit in one is merged
server-side (crdt:merge) and pushed to the other over SSE.

```bash
just host-scribe &                      # from repo root; editor on :3037
node scribe.mjs
bash to-gif.sh videos/scribe/*.webm ../../docs/media/scribe.gif 800 10
```

`node_modules/` and `videos/` are gitignored; only the scripts + the final gifs
are tracked.
