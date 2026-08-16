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
# from repo root: host/target/release/comp-host --component \
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

**jobs** — durable job-queue board; a burst marches Queued → Running → Done, a
flaky job retries with backoff then completes, a boom job dead-letters, then
Replay requeues it. The SSE board self-ticks.

```bash
just host-jobs &                        # from repo root; board on :3038
node jobs.mjs
bash to-gif.sh videos/jobs/*.webm ../../docs/media/jobs.gif 900 10
```

**jobs-golem** — the jobs queue on the wasmCloud v2 operator (k8s) with the live
Golem backend; jobs execute as durable Golem workers and land in Done with the
worker's climbing counter value. Records the real cluster board.

```bash
just k8s-jobs &                         # deploy on the v2 operator (see docs/apps/JOBS.md)
node jobs-golem.mjs                      # JOBS_URL defaults to the cluster DNS
bash to-gif.sh videos/jobs-golem/*.webm ../../docs/media/jobs-golem.gif 820 9
```

**arena** — multiplayer Connect Four; two panes on one game (Alice creates as
Red, Bob joins as Yellow), moves validated server-side and streamed to both
boards over SSE, red wins and the four-in-a-row glows.

```bash
just host-arena &                       # from repo root; game on :3039
node arena.mjs
bash to-gif.sh videos/arena/*.webm ../../docs/media/arena.gif 700 8
```

**tempo** — worktime logger (React + shadcn), recorded at a PHONE viewport to
show it's mobile-friendly; seeds a team via the API, then drives the SPA as a
project lead: Reports charts (recharts donut + bars), range + Everyone/Mine
toggles, and a pomodoro timer.

```bash
just host-tempo &                       # from repo root; builds the UI + serves :3040
node tempo.mjs                          # seeds data, then records the SPA (mobile)
bash to-gif.sh videos/tempo/*.webm ../../docs/media/tempo.gif 400 10
```

**mesh** — resilience playground in front of the real flaky upstream: a healthy
call, a 300ms response failing a 100ms SLO, then a burst that trips the breaker —
after which calls come back "shed — upstream not called" and the `shed` counter
climbs while `calls` stops. The cooldown runs out and one probe closes it again.

```bash
just host-mesh &                        # from repo root; SPA :3050, upstream :3051
node mesh.mjs
bash to-gif.sh videos/mesh/*.webm ../../docs/media/mesh.gif 820 10
```

**passkey** — passwordless WebAuthn sign-in, driven by Chromium's CDP **virtual
authenticator** (`WebAuthn.addVirtualAuthenticator`): a real CTAP2 authenticator
with a real key pair, minus the biometric prompt — which is the only reason this
one is recordable at all. Enrols "ada", adds a second device (a second virtual
authenticator, since the first is in `excludeCredentials`), signs out, then signs
back in with **no username** via the discoverable credential.

Restart the host before re-recording: its kv is in-memory, and an existing "ada"
makes the first enrolment correctly refuse without a session.

```bash
just host-passkey &                     # from repo root; SPA on :3053
node passkey.mjs                        # navigates to localhost:3053 (the RP ID)
bash to-gif.sh videos/passkey/*.webm ../../docs/media/passkey.gif 700 10
```

**studio** — the composition studio (React + **@xyflow/react**) with the repo's own
109 components in the palette: place four, drag export handles onto matching import
handles (the plan flips from "Unsatisfied (3)" to zero), flip through the three
emitted forms — `wac plug` script, `.wac` file, wasmCloud workload — and hit Compose
for a real composed component. Needs the palette seeded, which `host-studio` does.

```bash
just host-studio &                      # from repo root; SPA :3054, seeds 109 components
node studio.mjs
bash to-gif.sh videos/studio/*.webm ../../docs/media/studio.gif 780 6
```

**console** — the Holon console: sign in, the worklist, then the runs tab and one
run's whole history — both branches kept (the loser's 40 beside the winner's 100),
the gate's verdicts in order, and the capability the pool was missing. The only
recorder here that starts its **own** stack, because the console needs three things
behind it (the knowledge store it reads runs from, a platform to authenticate
against, and the composed component) where every other app needs one. The run it
opens is written by `comp-trace-seed` through `trace.rs` — the driver's own code
path — so what is on screen is the shape a real run records (ADR-0092).

```bash
just compose-console                    # from repo root; needs docker for SurrealDB
node console.mjs                        # starts + tears down everything itself
bash to-gif.sh videos/console/*.webm ../../docs/media/console.gif 900 12
```

`node_modules/` and `videos/` are gitignored; only the scripts + the final gifs
are tracked.
