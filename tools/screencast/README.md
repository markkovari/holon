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

`node_modules/` and `videos/` are gitignored; only the scripts + the final gifs
are tracked.
