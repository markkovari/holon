# --------------------------------------------------------------------------
# The end-to-end checks, one per app.
#
# These drive a real fleet — a host, an ingress, a composed artifact — and assert
# what it serves. They are the slow half of `just ci-local`, and the reason it is
# worth having a fast half.
# --------------------------------------------------------------------------

# The clinic's behavioural gates — the four checks `holon goal run` judges the
# decomposed clinic goal by, run the same way it runs them.
#
# Each one builds the component, composes it with `plug` (derived from its own
# imports, not from a chain written here), starts it on comp-host and drives real
# HTTP against it. `access` is EXPECTED to fail on a clean tree: `src/access.rs` is
# the stub the goal exists to replace, and a check that passes before the work is
# done cannot judge the work.
e2e-clinic: build
    #!/usr/bin/env bash
    set -uo pipefail
    (cd host && cargo build --release --quiet --bin comp-host)
    (cd reconciler && cargo build --release --quiet --bin comp-plug)
    export COMP_HOST="$PWD/host/target/release/comp-host"
    export COMP_PLUG="$PWD/reconciler/target/release/comp-plug"
    # The halves that are written must pass; anything else here is a real
    # regression.
    failed=0
    for gate in e2e-owners e2e-visits; do
      printf '%-12s ' "$gate"
      bash "components/clinic-domain/$gate.sh" 2>&1 | tail -1 || failed=1
    done
    # The two unwritten parts, judged separately: on a clean tree their FAILURE is
    # the correct outcome, and an exit code that says "broken" every time teaches
    # everyone to ignore it. When one passes, that part has been written.
    # The unwritten parts and the join that covers them. On a clean tree their
    # FAILURE is the correct outcome — a check that passes before the work is done
    # cannot judge the work — so they do not set the exit code.
    for gate in e2e e2e-access e2e-reports; do
      printf '%-12s ' "$gate"
      if bash "components/clinic-domain/$gate.sh" 2>&1 | tail -1; then
        echo "             ^ $gate now passes — that part is written"
      fi
    done
    exit $failed

# The poll's browser suite: Playwright against the real stack.
#
#   playwright -> poll-domain (wasm) -> record-store + svg-chart + qr-encode (wasm)
#
# A browser and not curl, because what is being asserted needs one: ONE VOTE PER
# BROWSER is a cookie rule, so it takes two cookie jars, and a single HTTP client
# either replays the cookie it was just handed or never sends one — both wrong, both
# green. And a chart is only right if the PAGE embedded it; `<svg>` in a response
# body proves the renderer, `<svg>` in the DOM proves the app.
#
# Fails loudly when a prerequisite is missing rather than skipping: a browser suite
# that "passes" because the app never started is the worst outcome there is.
e2e-poll: compose-poll build-reconciler
    cd host && cargo build --release --quiet --bin comp-host
    cd examples/poll && npm ci && npx playwright install --with-deps chromium && npx playwright test

# The console's browser suite: Playwright against the real stack.
#
#   playwright -> console-domain (wasm) -> knowledge:graph (wasm) -> SurrealDB
#
# Nothing below the browser is stubbed. `globalSetup` starts a pinned SurrealDB,
# seeds one run through `comp-trace-seed` (the SAME `trace.rs` a run calls, so a
# schema drift fails here too), stands in for the platform's login, and runs
# `comp-host` on the composed component.
#
# Fails loudly when a prerequisite is missing rather than skipping: a browser
# suite that "passes" because the app never started is the worst outcome there
# is.
e2e-console: compose-console
    @cd reconciler && cargo build --release --quiet --bin comp-trace-seed
    cd examples/console && npm ci && npx playwright install --with-deps chromium && npx playwright test

# conduit e2e: build the composed app + native host, then a Rust test that spawns
# the host and drives the full API (users/profiles/articles/comments/favorites).
e2e-conduit: compose-conduit
    cd host && cargo build --release --bin comp-host
    cd examples/conduit && cargo test --release

# Saga e2e: compose + build host + a Rust test that spawns the host and drives
# commit, compensation, and (NATS) resume-after-restart over real HTTP.
e2e-saga: compose-saga
    cd host && cargo build --release --bin comp-host
    cd examples/saga && cargo test --release

# Realtime e2e: compose + build host + a Rust test that posts a message and
# proves a SEPARATE held-open SSE connection receives it live.
e2e-pulse: compose-pulse
    cd host && cargo build --release --bin comp-host
    cd examples/pulse && cargo test --release

# Worktime e2e: compose + build host + a Rust test — admin creates projects +
# categories, members log entries + a pomodoro timer, and the report aggregates
# by project/category over a range with RBAC scope (member=own, manager=all).
e2e-tempo: compose-tempo
    cd host && cargo build --release --bin comp-host
    cd examples/tempo && cargo test --release

# Booking e2e: owner creates a resource + availability; a member books a slot;
# a SECOND booking of the same slot is rejected (no double-book); concurrent
# attempts leave exactly one booking; a recurrence expands to N instances; and
# a booking exports to a valid .ics.
e2e-booked: compose-booked
    cd host && cargo build --release --bin comp-host
    cd examples/booked && cargo test --release

# Ticketing e2e: a rider buys tickets; a validator validates — a single is
# consumed by one scan (a second is rejected); a duration ticket activates with
# a remaining window; CONCURRENT scans of one single ticket accept exactly once;
# a fabricated code is rejected; and a ticket renders a valid QR SVG.
e2e-transit: compose-transit
    cd host && cargo build --release --bin comp-host
    cd examples/transit && cargo test --release

# Dashboards e2e: a fresh account is seeded a demo dashboard; each panel renders
# to a valid SVG per kind (bar/line/donut/sparkline); a new panel round-trips;
# and one account cannot read another's dashboards.
e2e-dashboards: compose-dashboards
    cd host && cargo build --release --bin comp-host
    cd examples/dashboards && cargo test --release

# Gateway e2e: a token bucket allows `capacity` then 429s then refills; GCRA
# admits a burst then spaces with an exact retry-after; concurrent hits on one
# key admit exactly `capacity` (durable per-key CAS = a single-writer worker);
# and a batch coalesces submits and flushes atomically with per-item results.
e2e-gate: compose-gate
    cd host && cargo build --release --bin comp-host
    cd examples/gate && cargo test --release

# Bookkeeping e2e: a balanced entry posts; an UNBALANCED entry is rejected; the
# trial balance's debits equal its credits; the balance sheet balances
# (assets = liabilities + equity + net income); and a statements PDF renders.
e2e-books: compose-books
    cd host && cargo build --release --bin comp-host
    cd examples/books && cargo test --release

# Stash e2e: notes CRUD; the export is a valid ZIP (PK header + intact central
# directory) containing a .md per note, an index.csv, and a manifest.json.
e2e-stash: compose-stash
    cd host && cargo build --release --bin comp-host
    cd examples/stash && cargo test --release

# Payee-book e2e: a valid IBAN is accepted (stored normalized + country); a
# bad-checksum / wrong-length / bad-country IBAN is rejected with the reason;
# /verify returns the parsed country + grouped form; and ownership is enforced.
e2e-payees: compose-payees
    cd host && cargo build --release --bin comp-host
    cd examples/payees && cargo test --release

# Learning e2e: an instructor creates a course + quiz; a student enrolls and
# submits, which auto-grades (quiz:grade); the instructor gradebook reflects it
# consistently; a certificate issues once every quiz is passed; and the student's
# progress reconciles with the gradebook.
e2e-lms: compose-lms
    cd host && cargo build --release --bin comp-host
    cd examples/lms && cargo test --release

# Game e2e: a host starts a game; two players join and answer at different speeds;
# reveal grades speed-weighted (faster-correct > slower-correct > wrong=0); the
# leaderboard ranks correctly; and the game ends on a podium.
e2e-buzz: compose-buzz
    cd host && cargo build --release --bin comp-host
    cd examples/buzz && cargo test --release

e2e-photosocial: compose-photosocial
    cd host && cargo build --release --bin comp-host
    cd examples/photosocial && cargo test --release

# Resilience e2e against the REAL flaky upstream: retries ride out a two-request
# blip; `failure_threshold` failures trip the breaker and while it is OPEN the
# upstream's own hit counter proves it is never dialled; a half-open probe closes
# it again; a response slower than `slo_ms` counts as failed despite its 200; an
# unreachable upstream trips the breaker but a missing route (our config bug)
# does not.
e2e-mesh: compose-mesh
    cd host && cargo build --release --bin comp-host
    cd examples/mesh && cargo build --release --bin flaky && cargo test --release

# Passkey e2e with a VIRTUAL AUTHENTICATOR: the test holds a P-256 key and
# performs the real ceremonies over HTTP (CBOR attestation object, COSE key,
# DER-encoded ECDSA over authData || sha256(clientDataJSON)). It registers, logs
# in, and then proves each check bites: a replayed challenge, a phishing origin,
# a credential from another RP, a signature from the wrong key, and a counter
# that went backwards are all refused — by reason.
e2e-passkey: compose-passkey
    cd host && cargo build --release --bin comp-host
    cd examples/passkey && cargo test --release

# Platform e2e: sign in, upload components, refuse a deploy with no digest
# (ADR-0006), record a push, then save under BOTH strategies and assert the
# rendered manifests — namespace, one hostInterfaces entry per interface, the
# isolation stamp, digest pinning. Needs no cluster and no NATS.
e2e-platform: compose-platform build-reconciler
    cd host && cargo build --release --bin comp-host
    cd examples/platform && cargo test --release

# Studio e2e: reflect real components over HTTP, refuse an illegal edge (wac's
# subtype check says no), plan a two-level build in the right order, emit all
# three forms, and COMPOSE FOR REAL — then prove the composed component is the
# same artifact `wac plug` writes and that the host will actually serve it.
e2e-studio: compose-studio
    cd host && cargo build --release --bin comp-host
    cd examples/studio && cargo test --release

# Game e2e: compose + build host + a Rust test that plays a full game — create,
# join, turn/seat/illegal-move rejection, a scripted win, and a concurrent-move
# revision conflict.
e2e-arena: compose-arena
    cd host && cargo build --release --bin comp-host
    cd examples/arena && cargo test --release

# Job-queue e2e: compose + build host + a Rust test that enqueues jobs, drives
# ticks, and proves success / retry-then-succeed / dead-letter / replay + the
# exactly-once enqueue key.
e2e-jobs: compose-jobs
    cd host && cargo build --release --bin comp-host
    cd examples/jobs && cargo test --release

# Collaborative-editor e2e: compose + build host + a Rust test proving two
# concurrent edits merge (both survive) and a live SSE connection sees them.
e2e-scribe: compose-scribe
    cd host && cargo build --release --bin comp-host
    cd examples/scribe && cargo test --release

# Reliability e2e: compose + build host + a Rust test that enqueues events,
# proves they deliver (acked), then takes the sink down and proves an event
# retries and drops to the dead-letter tray, and that Replay requeues it.
e2e-pipeline: compose-pipeline
    cd host && cargo build --release --bin comp-host
    cd examples/pipeline && cargo test --release

# Rollout e2e: compose + build host + a Rust test that sets a 30% rule and
# proves (a) a subject is STICKY across repeated evals, (b) raising the
# percentage never turns an already-on subject off, and (c) a rule flip made by
# one request reaches a SEPARATE held-open SSE connection live.
e2e-flags: compose-flags
    cd host && cargo build --release --bin comp-host
    cd examples/flags && cargo test --release

# Experiment e2e: compose + build host + a Rust test that defines a 50/25/25
# experiment and proves (a) assignment is STICKY per subject, (b) two different
# subjects can land in different arms, (c) the ~50/25/25 split holds across a
# cohort, (d) conversions attribute to the right arm's rate, and (e) an outcome
# recorded by one request reaches a SEPARATE held-open SSE connection live.
e2e-abtest: compose-abtest
    cd host && cargo build --release --bin comp-host
    cd examples/abtest && cargo test --release

# Search e2e: compose + build host + a Rust test that seeds the corpus and
# proves ranked results (a rare term ranks its doc first), all-mode intersection
# shrinks the set, a tag facet restricts hits, and the cache serves a repeat
# query (hit-ratio rises).
e2e-search: compose-search
    cd host && cargo build --release --bin comp-host
    cd examples/search && cargo test --release

# Throttle e2e: compose + build host + a Rust test that proves N allowed then a
# 429 at the ceiling, a quota `remaining` that decrements, lockout after a burst
# of failures, and a verdict reaching a SEPARATE held-open SSE connection.
e2e-ratelimit: compose-ratelimit
    cd host && cargo build --release --bin comp-host
    cd examples/ratelimit && cargo test --release

# Drop e2e: compose + build host + a Rust test that proves a ticket is minted
# for an allowed type, an oversized/blocked type is rejected at ticket time, a
# redeemed ticket stores bytes, and a signed download link round-trips the bytes
# while a tampered signature is refused.
e2e-drop: compose-drop
    cd host && cargo build --release --bin comp-host
    cd examples/drop && cargo test --release

# Report e2e: compose + build host + a Rust test that imports a CSV with a mix
# of valid + invalid rows (proving typed validation splits them with per-field
# errors), pages the clean set through the opaque cursor, and exports it back to
# CSV through the same codec (round-trip).
e2e-report: compose-report
    cd host && cargo build --release --bin comp-host
    cd examples/report && cargo test --release

# Authgate e2e: compose + build host + a Rust test that provisions a secret,
# derives a valid TOTP code from it, activates enrollment, logs in with a live
# code (rejecting a wrong one), and burns a single-use recovery code (rejecting
# its reuse) — proving the full challenge-response lifecycle.
e2e-authgate: compose-authgate
    cd host && cargo build --release --bin comp-host
    cd examples/authgate && cargo test --release

# Compose paste-bin (docs/apps/PASTE.md — a paste/gist bin) with the pure-compute
# transform chain (validate + pii-redact + markdown + slug) plus the one
# stateful piece (records). No auth. Remaining imports are WASI (kv).
# Compose events-domain (free event ticketing) with everything it needs: auth-guard
# for the whole authorisation table, record-store for the three collections,
# quota-meter for capacity held ATOMICALLY, qr for the attendee's code, fsm-workflow
# for the ticket lifecycle, id-generate for a code that cannot be guessed.
# The notification capabilities, proven against a REAL mailbox.
#
# `comp-host` wires no wasi:sockets, so a component cannot speak SMTP and MailHog
# ingests nothing else. `comp-mailrelay` is the bridge; the gate starts both itself
# so it does not fail on a clean machine as "your email code is broken".
#
#   go install github.com/mailhog/MailHog@latest
e2e-notify: build
    cd reconciler && cargo build --release --bin comp-mailrelay
    COMP_HOST="$PWD/host/target/release/comp-host" \
    COMP_PLUG="$PWD/reconciler/target/release/comp-plug" \
      bash components/notify-probe/e2e.sh

# Paste e2e: compose + build host + a Rust test that proves an empty body is
# rejected (validate), PII in the body is masked BEFORE storage (the raw email
# never appears in the stored/raw output), Markdown renders to sanitized HTML
# (a <script> is escaped, not executed), and duplicate titles get distinct
# slugs.
e2e-paste: compose-paste
    cd host && cargo build --release --bin comp-host
    cd examples/paste && cargo test --release

# Binder e2e: a fenced model answer becomes a typed card; an incomplete one is
# flagged rather than defaulted; a photo that is not a card is refused; and the
# money is checked as ARITHMETIC — buy 2 @ 10.00, buy 1 @ 40.00, sell 1 @ 30.00
# realises 20.00 under FIFO and 10.00 under average cost, so the assertion fails
# for a plausible wrong answer rather than only for a broken one.
e2e-binder: compose-binder
    cd host && cargo build --release --bin comp-host
    cd reconciler && cargo build --release --bin comp-plug
    cd examples/binder && cargo test --release

# stale-sweep tick, and the AI thread summary.
e2e-track: compose-track
    cd host && cargo build --release --bin comp-host
    cd examples/track && cargo test --release

# Status-page e2e: compose + build host + a Rust test that adds a self-probe
# (stays up) and a dead-port monitor, then proves the fsm walks up -> degraded
# (one failure) -> down (a second consecutive failure), with both transitions in
# the history log. Slow: monitors have a 10s minimum period, so it sleeps across
# a period to force the second probe.
e2e-status: compose-status
    cd host && cargo build --release --bin comp-host
    cd examples/status && cargo test --release

e2e-health-records: compose-health-records
	cd host && cargo build --release --bin comp-host
	cd examples/health-records && cargo test --release

e2e-freight-tracker: compose-freight-tracker
	cd host && cargo build --release --bin comp-host
	cd examples/freight-tracker && cargo test --release

e2e-smart-home: compose-smart-home
	cd host && cargo build --release --bin comp-host
	cd examples/smart-home && cargo test --release

e2e-academic-review: compose-academic-review
	cd host && cargo build --release --bin comp-host
	cd examples/academic-review && cargo test --release

e2e-real-estate-escrow: compose-real-estate-escrow
	cd host && cargo build --release --bin comp-host
	cd examples/real-estate-escrow && cargo test --release

e2e-device-radar: compose-device-radar
	cd host && cargo build --release --bin comp-host
	cd examples/device-radar && cargo test --release

# The binder's own e2e, run against a composition built in another LANGUAGE.
#
#     just e2e-binder-poly go portfolio-value
#     just e2e-binder-poly c  portfolio-value
#     just e2e-binder-poly js price-history
#     just e2e-binder-poly py price-history
#     just e2e-binder-poly go price-history
#
# `portfolio:value` (FIFO cost basis, realised and unrealised gain, the chart
# series) and `price:history` (carry-forward, staleness, absent-before-first-quote)
# are re-derived under `components/<capability>-<lang>`, built through
# `tools/build-polyglot.sh`, and swapped in for the Rust builds by filename.
# Nothing in `examples/binder/tests/binder.rs` is edited: the same 122 assertions
# judge whichever artifact satisfies the contract, which is the claim.
#
# The Rust build is put back afterwards even when the test fails — it is what
# `just build` produces and what everything else composes against.
e2e-binder-poly lang cap: build
    #!/usr/bin/env bash
    set -uo pipefail
    cd "{{justfile_directory()}}"
    ./tools/build-polyglot.sh {{lang}} {{cap}} || exit 1
    snake=$(echo {{cap}} | tr - _)
    cp "{{rel}}/$snake.wasm" "/tmp/$snake.rust.wasm"
    restore() {
      cp "/tmp/$snake.rust.wasm" "{{rel}}/$snake.wasm"
      just _derive binder-domain {{binder_composed}} >/dev/null
      echo "restored the Rust build of {{cap}}"
    }
    trap restore EXIT
    cp "components/target/${snake}_{{lang}}.wasm" "{{rel}}/$snake.wasm"
    just _derive binder-domain {{binder_composed}}
    (cd host && cargo build --release --quiet --bin comp-host)
    (cd examples/binder && cargo test --release)

# Every language, one after another. The table in
# components/portfolio-value-go/README.md is generated from exactly these runs.
e2e-binder-poly-all:
    just e2e-binder-poly c  portfolio-value
    just e2e-binder-poly go portfolio-value
    just e2e-binder-poly go price-history
    just e2e-binder-poly js price-history
    just e2e-binder-poly py price-history
