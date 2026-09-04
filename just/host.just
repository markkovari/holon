# --------------------------------------------------------------------------
# Running things on the native host, and the fleet recipes around it.
# --------------------------------------------------------------------------

# Run the poll app on the native host, in-memory kv.
#
#   just host-poll
#   open http://127.0.0.1:3057
host-poll: compose-poll
    cd host && cargo run --release --bin comp-host -- \
      --app poll --config default-tenant=poll \
      --component ../{{poll_composed}} --addr 127.0.0.1:3057

# Run the console on the native host. Needs `platform-url` pointing at a running
# platform, and — to author a goal — a forge repo and token, because the write
# path is a real pull request.
#
#   just host-console
#   open http://127.0.0.1:3055
host-console: compose-console
    #!/usr/bin/env bash
    set -euo pipefail
    # The run view reads the knowledge store DIRECTLY (ADR-0091 keeps run history
    # out of the control plane), so without this the console comes up, serves, and
    # shows no runs — which reads as "no runs happened" rather than as a console
    # that was never told where they are. `e2e-console` passed these all along;
    # only the recipe a PERSON runs by hand did not.
    #
    # The store is on loopback, which the host denies by default (ADR-0008).
    surreal=()
    if [ -n "${SURREAL_URL:-}" ]; then
      auth=${SURREAL_URL#*://}
      surreal=(--config "surreal-url=$SURREAL_URL" \
               --config "surreal-ns=${SURREAL_NS:-comp}" \
               --config "surreal-db=${SURREAL_DB:-goalmemory}" \
               --config "surreal-user=${SURREAL_USER:-root}" \
               --egress "$auth" --allow-private-egress)
    fi
    cd host && cargo run --release --bin comp-host -- \
      --app console --config-file ../examples/defaults.conf \
      --config default-tenant=console \
      --config platform-url=${PLATFORM_URL:-http://127.0.0.1:8080} \
      "${surreal[@]}" \
      --component ../{{console_composed}} --addr 0.0.0.0:3055

# Run the conduit app (docs/apps/CONDUIT.md rung 1) on the native Rust host, in-memory KV.
host-conduit: compose-conduit
    cd host && cargo run --release --bin comp-host -- \
      --app conduit --config-file ../examples/defaults.conf --config default-tenant=conduit \
      --component ../{{conduit_composed}} --addr 0.0.0.0:3008

# Run the saga app on the native Rust host. Use --kv nats to prove durability
# (state survives a restart); memory is fine for the happy/compensation paths.
host-saga: compose-saga
    cd host && cargo run --release --bin comp-host -- \
      --app saga --config-file ../examples/defaults.conf --config default-tenant=saga \
      --component ../{{saga_composed}} --addr 0.0.0.0:3012

# Run the chat app on the native Rust host + serve the two-pane SPA. Open two
# browser windows on http://127.0.0.1:3015 and watch messages stream live.
host-pulse: compose-pulse
    cd host && cargo run --release --bin comp-host -- \
      --app pulse --config-file ../examples/defaults.conf --config default-tenant=pulse \
      --component ../{{pulse_composed}} --addr 0.0.0.0:3015 \
      --static-dir ../examples/pulse/public

host-tempo: compose-tempo build-tempo-ui
    cd host && cargo run --release --bin comp-host -- \
      --app tempo --config-file ../examples/defaults.conf --config default-tenant=tempo \
      --component ../{{tempo_composed}} --addr 0.0.0.0:3040 \
      --static-dir ../examples/tempo/dist

# Run the booking app on the native host + serve the SPA on :3041. Register as
# `owner` to create resources + weekly availability; anyone else books free
# slots (no double-book), gets an .ics + a confirmation.
host-booked: compose-booked build-booked-ui
    cd host && cargo run --release --bin comp-host -- \
      --app booked --config-file ../examples/defaults.conf --config default-tenant=booked \
      --component ../{{booked_composed}} --addr 0.0.0.0:3041 \
      --static-dir ../examples/booked/dist

# Run the ticketing app on the native host + serve the SPA on :3042. Register as
# `rider` to buy fares (single / 60-min / 90-min / monthly) and show their QR;
# as `validator` to scan + validate with the device camera.
host-transit: compose-transit build-transit-ui
    cd host && cargo run --release --bin comp-host -- \
      --app transit --config-file ../examples/defaults.conf --config default-tenant=transit \
      --component ../{{transit_composed}} --addr 0.0.0.0:3042 \
      --static-dir ../examples/transit/dist

# Run the dashboards app on the native host + serve the SPA on :3043. Register a
# new account (seeded with a demo dashboard); add panels and see them rendered to
# SVG charts on the server — the frontend has no charting library.
host-dashboards: compose-dashboards build-dashboards-ui
    cd host && cargo run --release --bin comp-host -- \
      --app dashboards --config-file ../examples/defaults.conf --config default-tenant=dashboards \
      --component ../{{dashboards_composed}} --addr 0.0.0.0:3043 \
      --static-dir ../examples/dashboards/dist

# Run the gateway on the native host + serve the SPA on :3044. Fire bursts at the
# rate limiter (token bucket, 200/429), the throttle (GCRA smoothing), and submit
# items to watch a batch coalesce and flush — all per-key, durable state.
host-gate: compose-gate build-gate-ui
    cd host && cargo run --release --bin comp-host -- \
      --app gate --config-file ../examples/defaults.conf --config default-tenant=gate \
      --component ../{{gate_composed}} --addr 0.0.0.0:3044 \
      --static-dir ../examples/gate/dist

# Run the bookkeeping app on the native host + serve the SPA on :3045. Register
# a new account (seeded a demo chart + entries); post balanced journal entries
# and read the trial balance / P&L / balance sheet (+ PDF).
host-books: compose-books build-books-ui
    cd host && cargo run --release --bin comp-host -- \
      --app books --config-file ../examples/defaults.conf --config default-tenant=books \
      --component ../{{books_composed}} --addr 0.0.0.0:3045 \
      --static-dir ../examples/books/dist

# Run the note stash on the native host + serve the SPA on :3046. Register a new
# account (seeded demo notes), keep notes, and hit Export .zip to download them
# all as a real ZIP (Markdown + index.csv + manifest.json).
host-stash: compose-stash build-stash-ui
    cd host && cargo run --release --bin comp-host -- \
      --app stash --config-file ../examples/defaults.conf --config default-tenant=stash \
      --component ../{{stash_composed}} --addr 0.0.0.0:3046 \
      --static-dir ../examples/stash/dist

# Run the payee book on the native host + serve the SPA on :3047. Register a new
# account (seeded demo payees); add payees — the IBAN is validated as you type
# (country length + mod-97 checksum) and a typo is refused with the reason.
host-payees: compose-payees build-payees-ui
    cd host && cargo run --release --bin comp-host -- \
      --app payees --config-file ../examples/defaults.conf --config default-tenant=payees \
      --component ../{{payees_composed}} --addr 0.0.0.0:3047 \
      --static-dir ../examples/payees/dist

# Run the learning platform on the native host + serve the SPA on :3048. Register
# as `instructor` (creates courses/lessons/quizzes; seeded a demo course) or as
# `student` (enroll, take auto-graded quizzes, see progress + certificate).
host-lms: compose-lms build-lms-ui
    cd host && cargo run --release --bin comp-host -- \
      --app lms --config-file ../examples/defaults.conf --config default-tenant=lms \
      --component ../{{lms_composed}} --addr 0.0.0.0:3048 \
      --static-dir ../examples/lms/dist

# Run the quiz game on the native host + serve the SPA on :3049. Sign in as a
# host to run a game (get a PIN), or open on other devices to Join with the PIN
# and a nickname; the host drives the questions and everyone buzzes in.
host-buzz: compose-buzz build-buzz-ui
    cd host && cargo run --release --bin comp-host -- \
      --app buzz --config-file ../examples/defaults.conf --config default-tenant=buzz \
      --component ../{{buzz_composed}} --addr 0.0.0.0:3049 \
      --static-dir ../examples/buzz/dist

host-photosocial: compose-photosocial
    cd host && cargo run --release --bin comp-host -- \
      --app photosocial --config-file ../examples/defaults.conf --config default-tenant=photosocial \
      --component ../{{photosocial_composed}} --addr 0.0.0.0:3055

# Run the resilience playground on the native host + serve the SPA on :3050, with
# the flaky upstream on :3051 (started here, killed on exit). Hammer the upstream
# with failures and watch the breaker trip — while it is OPEN the upstream's hit
# counter stops moving, because the request never leaves the host.
host-mesh: compose-mesh build-mesh-ui
    #!/usr/bin/env bash
    set -euo pipefail
    cd examples/mesh && cargo build --release --bin flaky
    ./target/release/flaky 127.0.0.1:3051 &
    UPSTREAM_PID=$!
    trap 'kill $UPSTREAM_PID 2>/dev/null || true' EXIT
    cd ../../host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=mesh --config routes='/upstream=http://127.0.0.1:3051/,/dead=http://127.0.0.1:3052/' \
      --component ../{{mesh_composed}} --addr 0.0.0.0:3050 \
      --static-dir ../examples/mesh/dist

# Run passwordless sign-in on the native host + serve the SPA on :3053. Pick a
# username and hit Create passkey — the browser prompts for Touch ID / Windows
# Hello / your phone, and there is no password anywhere in the flow. Then sign out
# and sign back in with the passkey (or with no username at all, if your
# authenticator stores discoverable credentials).
#
# The RP id + origin come from CONFIG, never the request — that is what makes the
# origin check meaningful. WebAuthn needs a secure context: http://localhost
# counts, a LAN IP does not, so use localhost (not 0.0.0.0:3053) in the browser.
host-passkey: compose-passkey build-passkey-ui
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=passkey --config rp-id=localhost --config origin=http://localhost:3053 \
      --component ../{{passkey_composed}} --addr 127.0.0.1:3053 \
      --static-dir ../examples/passkey/dist

# Run the studio on the native host + serve the SPA on :3054, then seed it with
# every component in the repo. Drag components onto the canvas, wire the handles
# (only type-compatible connections are allowed — that check is wac's own), and
# read off the wac plug script, the .wac file, and the wasmCloud workload. Hit
# Compose to download a real composed component.
host-studio: compose-studio build-studio-ui
    #!/usr/bin/env bash
    set -euo pipefail
    cd host && cargo build --release --bin comp-host
    ./target/release/comp-host \
      --config default-tenant=studio \
      --component ../{{studio_composed}} --addr 127.0.0.1:3054 \
      --static-dir ../examples/studio/dist &
    HOST_PID=$!
    trap 'kill $HOST_PID 2>/dev/null || true' EXIT
    for _ in $(seq 1 100); do curl -sf http://127.0.0.1:3054/ >/dev/null && break; sleep 0.2; done
    cd .. && just seed-studio
    echo "studio on http://127.0.0.1:3054"
    wait $HOST_PID

# The whole platform locally: NATS, the control plane on :8080, the reconciler, and
# one lattice node on :3401. Then drive it with `./cli/target/release/holon`:
#
#   comp login --url http://127.0.0.1:8080 --email you@example.com --password ... --register
#   comp component push components/target/gate_domain.composed.wasm --id gate
#   comp app create shop --component gate && comp app ls && comp app deploy <id>
host-platform: compose-platform build-reconciler
    #!/usr/bin/env bash
    set -euo pipefail
    SECRET=${PLATFORM_SECRET:-dev-secret}
    STATE=$(mktemp -d)
    cd host && cargo build --release --bin comp-host && cd ..
    cd cli && cargo build --release && cd ..
    nats-server -js -sd "$STATE/nats" -a 127.0.0.1 -p 4222 >"$STATE/nats.log" 2>&1 &
    trap 'kill %1 %2 %3 2>/dev/null || true' EXIT
    sleep 1
    ./host/target/release/comp-host --component {{platform_composed}} \
      --addr 127.0.0.1:8080 --kv sqlite --sqlite-path "$STATE/platform.db" \
      --tenant platform --app control-plane \
      --config applier-secret="$SECRET" --config ingress-suffix=apps.local &
    sleep 2
    ./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8080 \
      --secret "$SECRET" --nats-url nats://127.0.0.1:4222 --lattice dev --interval 3 &
    echo "platform on :8080 | node on :3401 | state in $STATE"
    ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4222 --node dev-1 \
      --lattice dev --addr 127.0.0.1:3401 --state-dir "$STATE/node" \
      --kv sqlite --sqlite-path "$STATE/node.db"

# Run the game on the native host + serve the SPA. Open two windows on
# http://127.0.0.1:3039 — create a game in one, join from the other, play live.
host-arena: compose-arena
    cd host && cargo run --release --bin comp-host -- \
      --config registry=registry.platform.svc.cluster.local:5000 --config cluster-suffix=svc.cluster.local --config registry="$REG" --config cluster-suffix=svc.cluster.local \
      --app arena --config-file ../examples/defaults.conf --config default-tenant=arena \
      --component ../{{arena_composed}} --addr 0.0.0.0:3039 \
      --static-dir ../examples/arena/public

# Run the job queue on the native host + serve the board SPA. Open
# http://127.0.0.1:3038: enqueue jobs, watch them run/retry/dead-letter live,
# replay from the DLQ. CFG tunes the outbox: 3 attempts, 1s base backoff.
host-jobs: compose-jobs
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=jobs --config max-attempts=2 --config base-backoff=1 \
      --component ../{{jobs_composed}} --addr 0.0.0.0:3038 \
      --static-dir ../examples/jobs/public

# Run the collaborative editor on the native host + serve the two-pane SPA. Open
# two windows on http://127.0.0.1:3037 and edit the same doc — edits merge and
# stream live to both.
host-scribe: compose-scribe
    cd host && cargo run --release --bin comp-host -- \
      --app scribe --config-file ../examples/defaults.conf --config default-tenant=scribe \
      --component ../{{scribe_composed}} --addr 0.0.0.0:3037 \
      --static-dir ../examples/scribe/public

# Run the pipeline board on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3016: POST events, toggle the sink down, watch retries drop
# to the dead-letter tray, then Replay them — live over SSE.
host-pipeline: compose-pipeline
    cd host && cargo run --release --bin comp-host -- \
      --app pipeline --config-file ../examples/defaults.conf --config default-tenant=pipeline \
      --component ../{{pipeline_composed}} --addr 0.0.0.0:3016 \
      --static-dir ../examples/pipeline/public

# Run the rollout console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3017: drag a flag to 30% and watch ~30 of 100 subject tiles
# light up instantly and stay sticky; trip the kill-switch — all dark at once.
host-flags: compose-flags
    cd host && cargo run --release --bin comp-host -- \
      --app flags --config-file ../examples/defaults.conf --config default-tenant=flags \
      --component ../{{flags_composed}} --addr 0.0.0.0:3017 \
      --static-dir ../examples/flags/public

# Run the experiment console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3018: define control/variant-a/variant-b weights, watch 100
# subjects split into arms (sticky as weights shift), fire conversions, and see
# the per-arm conversion-rate bars pull apart live.
host-abtest: compose-abtest
    cd host && cargo run --release --bin comp-host -- \
      --app abtest --config-file ../examples/defaults.conf --config default-tenant=abtest \
      --component ../{{abtest_composed}} --addr 0.0.0.0:3018 \
      --static-dir ../examples/abtest/public

# Run the search console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3019: type in the box, watch ranked hits narrow live, click a
# facet chip to filter, and watch the cache hit-ratio climb on repeat queries.
host-search: compose-search
    cd host && cargo run --release --bin comp-host -- \
      --app search --config-file ../examples/defaults.conf --config default-tenant=search \
      --component ../{{search_composed}} --addr 0.0.0.0:3019 \
      --static-dir ../examples/search/public

# Run the throttle wall on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3020: hold the hammer button, watch the attempt bar hit the
# ceiling and the key LOCK with a countdown, and the quota gauge drain — live.
# CFG_MAX_ATTEMPTS / CFG_LOCKOUT_WINDOW tune the wall (defaults 5 / 300s).
host-ratelimit: compose-ratelimit
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=throttle --config max-attempts=10 --config lockout-window=15 \
      --component ../{{throttle_composed}} --addr 0.0.0.0:3020 \
      --static-dir ../examples/ratelimit/public

# Run the drop-box on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3021: pick a file, watch it ask for a ticket (the policy
# answer), PUT the bytes straight to storage, then get a signed download link.
# CFG_ALLOWED_TYPES / CFG_MAX_SIZE tune the gate (defaults: all types / 10 MiB).
host-drop: compose-drop
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=drop --config allowed-types=text/plain,image/png --config max-size=1048576 \
      --component ../{{drop_composed}} --addr 0.0.0.0:3021 \
      --static-dir ../examples/drop/public

# Run the CSV import/report tool on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3022: paste a CSV, watch valid rows import and bad rows come
# back with per-field errors, page the clean report, then export it back to CSV.
host-report: compose-report
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=report \
      --component ../{{report_composed}} --addr 0.0.0.0:3022 \
      --static-dir ../examples/report/public

# Run the 2FA authgate on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3023: enroll an account (scan the QR / copy the secret into an
# authenticator app), activate with the first code, then log in with a live code
# or burn a recovery code. CFG_MASTER_KEY seals the TOTP secret in the vault.
host-authgate: compose-authgate
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=authgate --config master-key=bWZhLWRlbW8tbWFzdGVyLWtleS0zMi1ieXRlcyEhISE= \
      --component ../{{authgate_composed}} --addr 0.0.0.0:3023 \
      --static-dir ../examples/authgate/public

# The ticketing app on :3230, SPA included.
host-events: compose-events build-events-ui
    cd host && cargo run --release --bin comp-host -- \
      --app events --config-file ../examples/defaults.conf --config default-tenant=events \
      --component ../{{events_composed}} --addr 0.0.0.0:3230 \
      --static-dir ../examples/events/dist

# Run the paste bin on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3024: paste Markdown (with an email or card number in it),
# submit, and watch the PII get masked at ingest and the Markdown render to safe
# HTML on view — a pure-compute pipeline with one stateful step.
host-paste: compose-paste
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=paste \
      --component ../{{paste_composed}} --addr 0.0.0.0:3024 \
      --static-dir ../examples/paste/public

# Run the project tracker on the native Rust host. The SPA is BAKED into the
# component (track-assets) — no --static-dir. Open http://127.0.0.1:3025:
# register (first user as admin), create a project, file issues, move them
# across the board, comment, watch the activity feed stream live over SSE, and
# summarize a thread with AI.
host-track: compose-track
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=track \
      --component ../{{track_composed}} --addr 0.0.0.0:3025

# Run the grocery shop app on the native Rust host. The React SPA is BAKED into
# the component (grocery-assets) — no --static-dir. Real barcode:read WASI capability.
host-grocery: compose-grocery
    cd host && \
      cargo run --release --bin comp-host -- \
        --app grocery --config default-tenant=grocery \
      --component ../{{grocery_composed}} --addr 0.0.0.0:3055

# Run the binder on the native host. Bound to 0.0.0.0 on purpose: this is the app
# you open from another machine on the tailnet, so `http://<this-host>:3210` works
# without a tunnel. The store is in memory — restart it and the collection is gone,
# which is the right default for something you are trying out.
host-binder: compose-binder build-binder-ui
    cd host && cargo run --release --bin comp-host -- \
      --app binder --component ../{{binder_composed}} --addr 0.0.0.0:3210 \
      --config default-tenant=binder --static-dir ../examples/binder/dist \
      --config vision:base-url=http://127.0.0.1:8787 \
      --egress 127.0.0.1:8787 --allow-private-egress

# Run the composed vet-domain wasm under the NATIVE Rust host (wasmtime). No
# Node, no wasmCloud — `host/` is its own native binary that serves the
# component's HTTP and satisfies its keyvalue/config imports in-process.
host: compose-vet
    cd host && cargo run --release --bin comp-host -- --component ../{{vet_composed}} --addr 127.0.0.1:3007

# Run the FULL-PARITY app on the native host + serve the built React SPA. One
# Rust binary = UI + API. The whole vet-clinic, no Node. (--kv memory default.)
host-full: compose-vet-full
    cd host && cargo run --release --bin comp-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public

# Same, persisted to Redis (any redis-compatible server, e.g. valkey :6379).
host-redis: compose-vet-full
    cd host && cargo run --release --bin comp-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public \
      --kv redis --redis-url redis://127.0.0.1:6379

# Run the helpdesk app (docs/apps/HELPDESK.md rung 1) on the native host, persisted to
# NATS JetStream KV. Same bytes the jco example serves — different host.
host-helpdesk: compose-helpdesk
    cd host && cargo run --release --bin comp-host -- \
      --app helpdesk --config-file ../examples/defaults.conf --config default-tenant=helpdesk \
      --component ../{{helpdesk_composed}} --addr 0.0.0.0:3007 \
      --static-dir ../examples/jco-helpdesk/public \
      --kv nats --nats-url 127.0.0.1:4222

# Same, persisted to NATS JetStream KV (:4222 by default).
host-nats: compose-vet-full
    cd host && cargo run --release --bin comp-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public \
      --kv nats --nats-url 127.0.0.1:4222

# Run the whole eshop (identity/catalog/basket/ordering/payment + gateway with
# the embedded storefront) on native hosts over a shared NATS at :4222.
# Gateway/storefront: http://127.0.0.1:3100 — smoke: examples/eshop/smoke.sh
host-eshop: compose-eshop
    examples/eshop/run-local.sh

eshop_reg := env_var_or_default("ESHOP_REG", "localhost:30500")

# Run the composed link-shortener under the native host.
host-shortlink: compose-shortlink
    cd host && cargo run --release --bin comp-host -- --component ../{{shortlink_composed}} --addr 127.0.0.1:3008

# Run the composed dev-portal under the native host.
host-portal: compose-portal
    cd host && cargo run --release --bin comp-host -- --component ../{{portal_composed}} --addr 127.0.0.1:3009

# Run the composed webhook-relay under the native host.
host-relay: compose-relay
    cd host && cargo run --release --bin comp-host -- --component ../{{relay_composed}} --addr 127.0.0.1:3010

# Run the composed billing-ledger under the native host.
host-ledger: compose-ledger
    cd host && cargo run --release --bin comp-host -- --component ../{{ledger_composed}} --addr 127.0.0.1:3011

# Run the composed status-page under the native host. Open
# http://127.0.0.1:3012: add a monitor (url + period >= 10s), then POST
# /api/tick to probe — the page shows each monitor's up/degraded/down state and
# its fsm transition history (up -> degraded -> down needs TWO failures).
host-status: compose-status
    cd host && cargo run --release --bin comp-host -- --component ../{{statuspage_composed}} --addr 127.0.0.1:3012

host-graphviz: compose-graphviz
    cd host && cargo run --release --bin comp-host -- \
      --app graphviz --config-file ../examples/defaults.conf --config default-tenant=graphviz \
      --component ../{{graphviz_composed}} --addr 0.0.0.0:3056

host-health-records: compose-health-records
	cd host && cargo run --release --bin comp-host -- \
	  --app health-records --config-file ../examples/defaults.conf --config default-tenant=health-records \
	  --component ../{{health_records_composed}} --addr 0.0.0.0:3055

host-freight-tracker: compose-freight-tracker
	cd host && cargo run --release --bin comp-host -- \
	  --app freight-tracker --config-file ../examples/defaults.conf --config default-tenant=freight-tracker \
	  --component ../{{freight_tracker_composed}} --addr 0.0.0.0:3055

host-smart-home: compose-smart-home
	cd host && cargo run --release --bin comp-host -- \
	  --app smart-home --config-file ../examples/defaults.conf --config default-tenant=smart-home \
	  --component ../{{smart_home_composed}} --addr 0.0.0.0:3055

host-academic-review: compose-academic-review
	cd host && cargo run --release --bin comp-host -- \
	  --app academic-review --config-file ../examples/defaults.conf --config default-tenant=academic-review \
	  --component ../{{academic_review_composed}} --addr 0.0.0.0:3055

host-real-estate-escrow: compose-real-estate-escrow
	cd host && cargo run --release --bin comp-host -- \
	  --app real-estate-escrow --config-file ../examples/defaults.conf --config default-tenant=real-estate-escrow \
	  --component ../{{real_estate_escrow_composed}} --addr 0.0.0.0:3055

host-device-radar: compose-device-radar
	cd host && cargo run --release --bin comp-host -- \
	  --app device-radar --config-file ../examples/defaults.conf --config default-tenant=device-radar \
	  --component ../{{device_radar_composed}} --addr 0.0.0.0:3055

host-desktop-notifier:
    cd host && cargo run --release --bin comp-host -- --app desktop-notifier --config-file ../examples/defaults.conf --config default-tenant=desktop-notifier --component ../components/target/desktop-notifier.composed.wasm --addr 0.0.0.0:3056

host-clipboard-sync:
    cd host && cargo run --release --bin comp-host -- --app clipboard-sync --config-file ../examples/defaults.conf --config default-tenant=clipboard-sync --component ../components/target/clipboard-sync.composed.wasm --addr 0.0.0.0:3056

host-pdf-generator:
    cd host && cargo run --release --bin comp-host -- --app pdf-generator --config-file ../examples/defaults.conf --config default-tenant=pdf-generator --component ../components/target/pdf-generator.composed.wasm --addr 0.0.0.0:3056

host-local-ai:
    cd host && cargo run --release --bin comp-host -- --app local-ai --config-file ../examples/defaults.conf --config default-tenant=local-ai --component ../components/target/local-ai.composed.wasm --addr 0.0.0.0:3056

host-docker-manager:
    cd host && cargo run --release --bin comp-host -- --app docker-manager --config-file ../examples/defaults.conf --config default-tenant=docker-manager --component ../components/target/docker-manager.composed.wasm --addr 0.0.0.0:3056

host-video-transcoder:
    cd host && cargo run --release --bin comp-host -- --app video-transcoder --config-file ../examples/defaults.conf --config default-tenant=video-transcoder --component ../components/target/video-transcoder.composed.wasm --addr 0.0.0.0:3056

host-lan-scanner:
    cd host && cargo run --release --bin comp-host -- --app lan-scanner --config-file ../examples/defaults.conf --config default-tenant=lan-scanner --component ../components/target/lan-scanner.composed.wasm --addr 0.0.0.0:3056

host-mdns-discoverer:
    cd host && cargo run --release --bin comp-host -- --app mdns-discoverer --config-file ../examples/defaults.conf --config default-tenant=mdns-discoverer --component ../components/target/mdns-discoverer.composed.wasm --addr 0.0.0.0:3056

host-fs-watcher:
    cd host && cargo run --release --bin comp-host -- --app fs-watcher --config-file ../examples/defaults.conf --config default-tenant=fs-watcher --component ../components/target/fs-watcher.composed.wasm --addr 0.0.0.0:3056

host-vpn-manager:
    cd host && cargo run --release --bin comp-host -- --app vpn-manager --config-file ../examples/defaults.conf --config default-tenant=vpn-manager --component ../components/target/vpn-manager.composed.wasm --addr 0.0.0.0:3056

host-image-optimizer:
    cd host && cargo run --release --bin comp-host -- --app image-optimizer --config-file ../examples/defaults.conf --config default-tenant=image-optimizer --component ../components/target/image-optimizer.composed.wasm --addr 0.0.0.0:3056

host-cron-scheduler:
    cd host && cargo run --release --bin comp-host -- --app cron-scheduler --config-file ../examples/defaults.conf --config default-tenant=cron-scheduler --component ../components/target/cron-scheduler.composed.wasm --addr 0.0.0.0:3056
