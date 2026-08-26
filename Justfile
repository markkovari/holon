# comp — WIT-first universal auth + RBAC. Task runner.
#
# Requires: wasm-tools, wkg, cargo-component, wac, docker compose.
# Runtime deploy additionally needs `wash` (wasmCloud host CLI, not bundled).

set dotenv-load := true

wit_dir := "wit"
components := "components"
rel := components / "target/wasm32-wasip2/release"
iot_scanner_composed := "components/target/iot-scanner.composed.wasm"
device_radar_composed := "components/target/device-radar.composed.wasm"
health_records_composed := "components/target/health-records.composed.wasm"
freight_tracker_composed := "components/target/freight-tracker.composed.wasm"
smart_home_composed := "components/target/smart-home.composed.wasm"
academic_review_composed := "components/target/academic-review.composed.wasm"
real_estate_escrow_composed := "components/target/real-estate-escrow.composed.wasm"
guard_wasm := rel / "auth_guard.wasm"
consumer_wasm := rel / "sample_consumer.wasm"
ratelimit_wasm := rel / "rate_limiter.wasm"
idempotency_wasm := rel / "idempotency_guard.wasm"
featureflags_wasm := rel / "feature_flags.wasm"
auditlog_wasm := rel / "audit_log.wasm"
notify_wasm := rel / "notify_dispatch.wasm"
webhook_wasm := rel / "webhook_ingest.wasm"
session_wasm := rel / "session_store.wasm"
config_wasm := rel / "config_store.wasm"
secrets_wasm := rel / "secrets_vault.wasm"
loginapp_wasm := rel / "login_app.wasm"
guard_composed := "components/target/auth_guard.composed.wasm"
webhook_composed := "components/target/webhook_ingest.composed.wasm"
login_composed := "components/target/login_app.composed.wasm"
vetdomain_wasm := rel / "vet_domain.wasm"
recordstore_wasm := rel / "record_store.wasm"
validate_wasm := rel / "validate.wasm"
searchindex_wasm := rel / "search_index.wasm"
vet_composed := "components/target/vet_domain.composed.wasm"
vet_full_composed := "components/target/vet_domain.full.composed.wasm"
vet_lattice := "components/target/vet_domain.lattice.wasm"
ai_composed := "components/target/llm_local.composed.wasm"
staticassets_wasm := rel / "static_assets.wasm"
shortlink_wasm := rel / "link_shortener.wasm"
shortlink_composed := "components/target/link_shortener.composed.wasm"
portal_wasm := rel / "dev_portal.wasm"
portal_composed := "components/target/dev_portal.composed.wasm"
relay_wasm := rel / "webhook_relay.wasm"
relay_composed := "components/target/webhook_relay.composed.wasm"
ledger_wasm := rel / "billing_ledger.wasm"
ledger_composed := "components/target/billing_ledger.composed.wasm"
statuspage_wasm := rel / "status_page.wasm"
statuspage_composed := "components/target/status_page.composed.wasm"
helpdesk_wasm := rel / "helpdesk_domain.wasm"
helpdesk_composed := "components/target/helpdesk_domain.composed.wasm"
conduit_wasm := rel / "conduit_domain.wasm"
conduit_composed := "components/target/conduit_domain.composed.wasm"
saga_wasm := rel / "saga_domain.wasm"
saga_composed := "components/target/saga_domain.composed.wasm"
pulse_wasm := rel / "pulse_domain.wasm"
pulse_composed := "components/target/pulse_domain.composed.wasm"
pipeline_wasm := rel / "pipeline_domain.wasm"
pipeline_composed := "components/target/pipeline_domain.composed.wasm"
flags_wasm := rel / "flags_domain.wasm"
flags_composed := "components/target/flags_domain.composed.wasm"
abtest_wasm := rel / "abtest_domain.wasm"
abtest_composed := "components/target/abtest_domain.composed.wasm"
search_wasm := rel / "search_domain.wasm"
search_composed := "components/target/search_domain.composed.wasm"
throttle_wasm := rel / "throttle_domain.wasm"
throttle_composed := "components/target/throttle_domain.composed.wasm"
drop_wasm := rel / "upload_drop.wasm"
drop_composed := "components/target/upload_drop.composed.wasm"
report_wasm := rel / "csv_report.wasm"
report_composed := "components/target/csv_report.composed.wasm"
authgate_wasm := rel / "mfa_authgate.wasm"
authgate_composed := "components/target/mfa_authgate.composed.wasm"
paste_wasm := rel / "paste_bin.wasm"
paste_composed := "components/target/paste_bin.composed.wasm"
track_wasm := rel / "track_domain.wasm"
track_composed := "components/target/track_domain.composed.wasm"
scribe_wasm := rel / "scribe_domain.wasm"
scribe_composed := "components/target/scribe_domain.composed.wasm"
jobs_wasm := rel / "jobs_domain.wasm"
jobs_composed := "components/target/jobs_domain.composed.wasm"
arena_wasm := rel / "arena_domain.wasm"
arena_composed := "components/target/arena_domain.composed.wasm"
tempo_wasm := rel / "tempo_domain.wasm"
tempo_composed := "components/target/tempo_domain.composed.wasm"
pdf_wasm := rel / "pdf.wasm"
booked_wasm := rel / "booked_domain.wasm"
booked_composed := "components/target/booked_domain.composed.wasm"
transit_wasm := rel / "transit_domain.wasm"
transit_composed := "components/target/transit_domain.composed.wasm"
dashboards_wasm := rel / "dashboards_domain.wasm"
dashboards_composed := "components/target/dashboards_domain.composed.wasm"
gate_wasm := rel / "gate_domain.wasm"
gate_composed := "components/target/gate_domain.composed.wasm"
console_composed := "components/target/console_domain.composed.wasm"
poll_composed := "components/target/poll_domain.composed.wasm"
books_wasm := rel / "books_domain.wasm"
books_composed := "components/target/books_domain.composed.wasm"
stash_wasm := rel / "stash_domain.wasm"
stash_composed := "components/target/stash_domain.composed.wasm"
payees_wasm := rel / "payees_domain.wasm"
payees_composed := "components/target/payees_domain.composed.wasm"
lms_wasm := rel / "lms_domain.wasm"
lms_composed := "components/target/lms_domain.composed.wasm"
buzz_wasm := rel / "buzz_domain.wasm"
buzz_composed := "components/target/buzz_domain.composed.wasm"
photosocial_wasm := rel / "photosocial_domain.wasm"
photosocial_composed := "components/target/photosocial_domain.composed.wasm"
mesh_wasm := rel / "mesh_domain.wasm"
mesh_composed := "components/target/mesh_domain.composed.wasm"
passkey_wasm := rel / "passkey_domain.wasm"
passkey_composed := "components/target/passkey_domain.composed.wasm"
platform_wasm := rel / "platform_domain.wasm"
platform_composed := "components/target/platform_domain.composed.wasm"
studio_wasm := rel / "studio_domain.wasm"
studio_composed := "components/target/studio_domain.composed.wasm"
ghcr_owner := env_var_or_default("GHCR_OWNER", "markkovari")
trackassets_wasm := rel / "track_assets.wasm"
eshopcatalog_wasm := rel / "eshop_catalog.wasm"
eshopcatalog_composed := "components/target/eshop_catalog.composed.wasm"
eshopbasket_composed := "components/target/eshop_basket.composed.wasm"
eshopordering_composed := "components/target/eshop_ordering.composed.wasm"
eshoppayment_composed := "components/target/eshop_payment.composed.wasm"
eshopidentity_composed := "components/target/eshop_identity.composed.wasm"
eshopgateway_composed := "components/target/eshop_gateway.composed.wasm"

# List available recipes.
default:
    @just --list

# Fetch + vendor WASI WIT dependencies into wit/deps (commits to git).
vendor:
    wkg wit fetch

# Validate the WIT contract resolves (no build).
wit-check:
    wasm-tools component wit {{wit_dir}}

# Build all components as WASI p2 components (wasm32-wasip2).
#
# Three steps, each for a reason:
#
# 1. cargo-component 0.21.1 HARDCODES `--target wasm32-wasip1` — it ignores both
#    `--target` and `[build] target` — so it is used only to GENERATE the bindings
#    (`check` is enough, no codegen).
# 2. a plain cargo build for wasm32-wasip2, where rustc + wasm-component-ld emit a
#    component directly with no wasi_snapshot_preview1 adapter in it.
# 3. stamp the name + producers section back on. `wasm-component-ld` writes
#    neither, where cargo-component's adapter path did, so without this every
#    artifact is anonymous: `wasm-tools metadata show` reports `unknown(0)` and
#    `wit:reflect` cannot read a component's own name. ~35 bytes per artifact, and
#    idempotent — re-running `build` doesn't accumulate sections.
# Every test in the repo, in every workspace.
#
# This exists because there was no such recipe, and that is exactly how
# `platform-domain` came to have a test target that HAD NEVER COMPILED — a test
# referenced a function nobody wrote, so all 34 unit tests in the component with
# the most logic in it were silently unrun, and anything added to them since
# would have been too. Nothing ran them, so nothing complained.
#
# `--no-run` first, deliberately: a target that fails to COMPILE is the failure
# mode that hides, because a suite reporting "ok" for the crates it managed to
# build looks identical to a suite where everything ran. Compiling everything up
# front turns that into a hard stop.
#
# The wasm components are native test targets here — the pure logic in them
# (codecs, key derivation, escaping, parsers) is testable without a runtime, and
# that is where most of it lives.
# Every workspace's tests.
#
# GOAL_STUBS are excluded, and that is not a workaround. A goal stub is a crate
# whose implementation is deliberately absent and whose held-out tests are its
# SPECIFICATION — `semver-range` is 11 lines returning false, with 8 tests
# describing what a range matcher must do, kept as a benchmark for the loop to
# attempt. Its tests are supposed to be red. Leaving them in the umbrella means
# `just test` is red forever, which teaches everyone to stop reading it; taking
# them out means the umbrella answers the question it is actually asked, "is the
# repository broken?", and the stubs are listed at the end as what they are.
#
# To attempt one: `holon goal run` with that crate's goal spec. To see it fail on
# purpose: `cargo test -p semver-range` in components/.
GOAL_STUBS := "semver-range"

test:
    #!/usr/bin/env bash
    set -euo pipefail
    excludes=""
    for c in {{GOAL_STUBS}}; do excludes="$excludes --exclude $c"; done
    # Binaries too, not just test targets. Several suites shell out to a built
    # binary — `comp-host`, `comp-plug`, the `holon` CLI — and assert it exists.
    # Without this the umbrella tells you to go and build it, which is a step it is
    # perfectly capable of taking itself.
    for ws in components host lattice cli reconciler; do
      echo "=== $ws: compiling test targets"
      if [ "$ws" = components ]; then
        # shellcheck disable=SC2086
        (cd "$ws" && cargo test --release --workspace --no-run $excludes)
      else
        (cd "$ws" && cargo build --release --bins && cargo test --release --workspace --no-run)
      fi
    done
    # Teed, so the skips can be counted afterwards. A skipped test reports as a
    # PASS, and that is the one number nobody should read casually: without Docker
    # the suites proving the knowledge loop, the contract negotiation and every
    # composed deployment all skip, and the umbrella still says everything passed.
    # Failing on a skip would be wrong — skipping is correct on a machine with no
    # database — so it is counted and named instead.
    log="$(mktemp -t comp-test-XXXX)"
    trap 'rm -f "$log"' EXIT
    for ws in components host lattice cli reconciler; do
      echo "=== $ws"
      if [ "$ws" = components ]; then
        # shellcheck disable=SC2086
        (cd "$ws" && cargo test --release --workspace $excludes) 2>&1 | tee -a "$log"
      else
        (cd "$ws" && cargo test --release --workspace) 2>&1 | tee -a "$log"
      fi
    done
    skipped=$(grep -c 'SKIPPED' "$log" || true)
    echo
    if [ "$skipped" -gt 0 ]; then
      echo "$skipped test(s) SKIPPED — green here does not mean these were verified:"
      grep 'SKIPPED' "$log" | sed 's/^/  /' | cut -c1-110
    else
      echo "nothing skipped: every suite that can run, ran."
    fi
    echo
    echo "open goals, not run above (their held-out tests are the spec, and fail on purpose):"
    for c in {{GOAL_STUBS}}; do echo "  components/$c — cargo test -p $c"; done


# The same, without the slow integration suites — for a quick check while editing.
test-fast:
    #!/usr/bin/env bash
    set -euo pipefail
    excludes=""
    for c in {{GOAL_STUBS}}; do excludes="$excludes --exclude $c"; done
    for ws in components host lattice cli; do
      echo "=== $ws"
      if [ "$ws" = components ]; then
        # shellcheck disable=SC2086
        (cd "$ws" && cargo test --release --workspace $excludes)
      else
        (cd "$ws" && cargo test --release --workspace)
      fi
    done
    (cd reconciler && cargo test --release --lib --bins)

# The unit of work here is an ARTIFACT, not the workspace.
#
# A no-op `just build` used to take 5.4s, of which 0.13s was the only part that
# produces a `.wasm`. The rest was a full host-target `cargo component check` (2.9s)
# and re-stamping all 203 artifacts whether or not cargo had touched them (0.8s) —
# which also rewrote 25 MB of unchanged files on every build, defeating anything
# downstream that keys on mtime.
#
# Both are now conditional on their actual input:
#
#   * the check pass exists to regenerate `bindings.rs` (gitignored, produced by
#     `cargo-component`), so it runs when a `.wit` is newer than the newest
#     generated binding, and not otherwise;
#   * an artifact is stamped when it is newer than its stamp marker.
#
# `just build force=1` does the whole thing regardless, which is what CI wants and
# what to reach for if anything looks stale.
build force="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{components}}
    marker=target/.build-stamps
    mkdir -p "$marker"

    # Bindings are generated by `cargo component`, not by `cargo build`. The trigger
    # is a WIT newer than the newest binding it produced — not "always", which is
    # what made a no-op build cost three seconds of host-target compilation.
    # Pruning `target/` is not tidiness: it is 7.1 GB, and walking it to look for
    # `.wit` files cost more than the build it was guarding.
    newest_wit=$(find . -path ./target -prune -o -name '*.wit' -newer "$marker/.wit-checked" -print 2>/dev/null | head -1 || true)
    if [ -n "{{force}}" ] || [ ! -f "$marker/.wit-checked" ] || [ -n "$newest_wit" ]; then
      cargo component check --release
      touch "$marker/.wit-checked"
    fi

    cargo build --release --target wasm32-wasip2

    # Stamp what cargo actually rebuilt. `wasm32-wasip2` artifacts come out
    # anonymous and `wit-reflect` asserts the name, so an unstamped artifact fails
    # its own tests — but a stamped one does not need stamping twice.
    rustv=$(rustc --version | cut -d' ' -f2)
    stamped=0; skipped=0
    for f in target/wasm32-wasip2/release/*.wasm; do
      name=$(basename "$f" .wasm | tr '_' '-')
      stamp="$marker/$name"
      if [ -z "{{force}}" ] && [ -f "$stamp" ] && [ ! "$f" -nt "$stamp" ]; then
        skipped=$((skipped+1)); continue
      fi
      wasm-tools metadata add --name "$name" --language "Rust=$rustv" "$f" -o "$f.named"
      mv "$f.named" "$f"
      touch "$stamp"
      stamped=$((stamped+1))
    done
    total=$((stamped + skipped))
    echo "built $total components (wasm32-wasip2, named, no preview1 adapter) — stamped $stamped, unchanged $skipped"

# Compose the rate-limiter AND audit-log into auth-guard with wac, satisfying
# auth-guard's `ratelimit:guard/limiter` + `audit:log/recorder` imports. Output
# is a single self-contained component.
# Compose ANY component with whatever it imports, derived rather than written.
#
# The 59 hand-written plug chains below each name their plugs; this asks the
# component instead. `reconciler/src/plug.rs` wraps `wac` as a library: read the
# built artifact's imports, find what exports those interfaces, compose each plug
# first (a plug that is not whole hoists its own imports into the result), and key
# the output by content. A component the loop builds is therefore runnable without
# anyone editing this file — which was the point.
#
#   just plug clinic-domain
#   just plug vet-domain
plug name: build
    @cd reconciler && cargo build --release --quiet --bin comp-plug
    @./reconciler/target/release/comp-plug {{name}}

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

# The capability graph: who imports what from whom, across every built component.
#
# Answers the question nobody can answer from memory at this size — "may I change
# this interface?" — with a number. records:store/store has 37 consumers; some have
# one. Derived from the artifacts, so it cannot drift from what the components
# actually do.
#
#   just capgraph            # regenerate docs/CAPABILITY-GRAPH.md
#   just capgraph json       # the same graph for tooling
# No `build` dependency on purpose: cargo's progress goes to stdout, so
# `just capgraph json | jq` was parsing compiler output. The tool says what to do
# when nothing is built.
capgraph format="md":
    @cd reconciler && cargo build --release --quiet --bin comp-capgraph
    @if [ "{{format}}" = "md" ]; then \
        ./reconciler/target/release/comp-capgraph --format md > docs/CAPABILITY-GRAPH.md; \
        echo "wrote docs/CAPABILITY-GRAPH.md"; \
     else \
        ./reconciler/target/release/comp-capgraph --format {{format}}; \
     fi

# "Do we already have something for this?" — the question ADR-0089 says a goal
# should ask before generating an implementation.
#
# No model: term overlap over each package's WIT header, with the capability graph
# breaking ties towards what applications already carry. Costs nothing, answers in
# a millisecond, and says WHICH terms matched so the ranking can be checked.
#
#   just capability "hash a password and issue a session token"
capability query:
    @cd reconciler && cargo build --release --quiet --bin comp-capgraph
    @./reconciler/target/release/comp-capgraph --find "{{query}}"

# Build the console SPA (Vite) straight into components/console-assets/static,
# which that component's build.rs walks and embeds. There is no separate dist:
# the console is served by `ui:assets` (the deployable path), not --static-dir.
build-console-ui:
    cd examples/console/ui && npm ci && npm run build

# Compose console-domain with what it imports: the embedded SPA (ui:assets) and
# a forge (git:forge) — because a goal's SPEC is prose in git, so authoring one
# from a browser is a pull request, and a component has no filesystem.
#
# It does NOT import the control plane's storage. `platform-domain` already
# serves /api and the CLI is a client of it; this is the second client. Two
# components independently knowing the storage layout is two places the control
# plane's invariants can break.
compose-console: build-console-ui compose
    @just _derive console-domain {{console_composed}}

# Compose the poll app: poll-domain + record-store + id-generate + svg-chart +
# qr-encode. The chain is DERIVED from the component's own imports, so adding a
# capability to the world does not mean editing this. Output imports only generic
# WASI (keyvalue, clocks, random), so any comp host runs it.
compose-poll: build
    @just _derive poll-domain {{poll_composed}}

# Run the poll app on the native host, in-memory kv.
#
#   just host-poll
#   open http://127.0.0.1:3057
host-poll: compose-poll
    cd host && cargo run --release --bin comp-host -- \
      --app poll --config default-tenant=poll \
      --component ../{{poll_composed}} --addr 127.0.0.1:3057

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

# Serve `/v1/messages` from `claude -p` instead of the Anthropic API.
#
# Runs the loop's inference on a Claude Code subscription rather than an API key.
# Nothing in the component graph changes: `anthropic-provider` already reads its
# base URL from `wasi:config`, which is the same swap point `mock-provider` uses.
#
# Each request spawns a fresh `claude -p`, so a generation's branches stay
# concurrent and context-isolated (ADR-0078, ADR-0091) — one shared conversation
# would hand every branch the same context and undo both.
#
#   just claude-shim &
#   COMP_FLEET_ALLOW_PRIVATE_EGRESS=1 \
#     holon goal run --anthropic-base-url http://127.0.0.1:8787 …
#
# The private-egress flag is required and deliberately not set here: the fleet
# blocks private ranges by default, and a base URL pointing at localhost is
# exactly what a prompt-injected run would aim for.
# THREE timeouts sit in a row and the one here is the lowest, which is why raising
# the obvious one changes nothing:
#
#   CLAUDE_TIMEOUT_MS   this shim kills `claude -p`         default 540s  <- lowest
#   anthropic:timeout   the provider waits for a first byte from --timeout
#   --timeout           the branch's whole budget           goal-run's TIMEOUT
#
# They must stay in that order. A shim cap ABOVE the provider timeout means the
# provider hangs up on a call the shim is still happily running; a branch budget
# below either means the branch dies mid-answer. Measured on a three-part goal: six
# branches came back `errored` with no files, every one of them at 540006-540010ms —
# the cap, to the millisecond — while the branch budget of 900s was never reached.
# A big file through `claude -p` takes 300-500s on an idle machine and considerably
# longer with a dozen of them running at once, so the default is too small for any
# part that writes more than a hundred lines.
#
#   CLAUDE_TIMEOUT_MS=1500000 just claude-shim &        # 25 min per call
#   … TIMEOUT=3000 just goal-run                        # 50 min per branch
claude-shim port="8787" model="":
    @CLAUDE_MODEL="{{model}}" PORT="{{port}}" node tools/claude-shim.mjs

# An OpenAI-compatible server (vLLM, llama.cpp, Ollama) behind /v1/messages.
#   OPENAI_BASE=http://csatapaci:8000/v1 just openai-shim
openai-shim port="8787" model="":
    @OPENAI_MODEL="{{model}}" PORT="{{port}}" HOST="${SHIM_HOST:-127.0.0.1}" node tools/openai-shim.mjs

gemini-shim port="8788" model="gemini-2.5-flash":
    @GEMINI_MODEL="{{model}}" PORT="{{port}}" node tools/gemini-shim.mjs

# Push the capability graph into the store the knowledge pool lives in (ADR-0091).
#
# The graph stops being a report a person reads and becomes rows a query can join
# against. `just lessons-for` below is the whole point of doing it.
#
# Rerun this after any build. It is a PROJECTION: comp-capgraph stays the source,
# nothing here is anybody's only copy, and dropping the six derived tables costs a
# second of recompute. What it must never touch is `memory` and `task` — the half
# that cannot be recomputed from anything — and that is enforced by the generation
# stamp plus a test in capgraph.rs, not by being careful.
#
# `comp`/`goalmemory` are not a preference — they are where the other half already
# is. `knowledge-graph` defaults its namespace to `comp` and `comp-goalrun` rewrites
# the memory app's database to `goalmemory`, so lessons, runs and capabilities all
# land there. This recipe defaulted to `holon`/`holon` and therefore wrote the
# capability graph into a database with nothing to join against: ADR-0091's whole
# claim, in production, was projecting into an empty room.
#
#   just capgraph-store                        # against the compose surreal
#   SURREAL_URL=… SURREAL_PASS=… just capgraph-store
capgraph-store:
    @cd reconciler && cargo build --release --quiet --bin comp-capgraph
    @url="${SURREAL_URL:-http://localhost:8000}"; \
     ns="${SURREAL_NS:-comp}"; db="${SURREAL_DB:-goalmemory}"; \
     user="${SURREAL_USER:-root}"; pass="${SURREAL_PASS:-root}"; \
     gen=$(date +%s); \
     curl -sS -u "$user:$pass" -H "Accept: application/json" \
       -H "surreal-ns: $ns" -H "surreal-db: $db" \
       --data-binary "DEFINE NAMESPACE IF NOT EXISTS $ns; USE NS $ns; DEFINE DATABASE IF NOT EXISTS $db;" \
       "$url/sql" > /dev/null; \
     ./reconciler/target/release/comp-capgraph --format surql --gen "$gen" \
       | curl -sS -u "$user:$pass" -H "Accept: application/json" \
           -H "surreal-ns: $ns" -H "surreal-db: $db" \
           --data-binary @- "$url/sql" \
       | grep -o '"status":"ERR"' | wc -l | tr -d ' ' | { \
           read errs; \
           if [ "$errs" != "0" ]; then echo "FAILED: $errs statement(s) rejected"; exit 1; fi; \
           echo "projection written at generation $gen"; \
         }

# What did previous runs learn about the interfaces THIS app imports?
#
# The query ADR-0091 exists to make possible, and the one that was not possible
# while the capability graph and the knowledge pool were separate stores. Nothing
# in it mentions the app's subject matter: `just lessons-for vet` finds a lesson
# about `csv:codec/codec` because the vet app imports that interface, not because
# a veterinary clinic and a CSV parser share any wording. That last part is what
# killed two paid runs before this existed (ADR-0090).
#
# Run `just capgraph-store` first, or the graph half of the join is empty.
#
# TRAVERSED, not scanned. The first version of this read `memory WHERE tags
# CONTAINSANY $ifaces`, which is a full table scan of the one half ADR-0091
# measured as not scaling — 55ms at 200k lessons, against ~12ms for the edge. The
# projection now writes `lesson -about-> interface`, so this walks from the app to
# its interfaces to their lessons and touches nothing else.
#
#   just lessons-for vet
lessons-for app:
    @url="${SURREAL_URL:-http://localhost:8000}"; \
     ns="${SURREAL_NS:-comp}"; db="${SURREAL_DB:-goalmemory}"; \
     user="${SURREAL_USER:-root}"; pass="${SURREAL_PASS:-root}"; \
     printf 'LET $ls = (SELECT VALUE array::distinct(array::flatten(->carries->artifact->imports->interface<-about<-memory)) FROM ONLY app:%s{{app}}%s);\nSELECT ns, text, array::distinct(->about->interface.name) AS matched FROM $ls;\n' '⟨' '⟩' \
       | curl -sS -u "$user:$pass" -H "Accept: application/json" \
           -H "surreal-ns: $ns" -H "surreal-db: $db" \
           --data-binary @- "$url/sql" \
       | jq -r 'last | if .status != "OK" then "query failed: \(.result)" \
                elif (.result | length) == 0 then "no lessons about anything {{app}} imports (yet)" \
                else (.result[] | "  [\(.ns)] \(.text)\n      via \(.matched | join(", "))") end'

# What it WOULD plug, and what nothing exports. For a composition that is missing
# something.
plug-wiring name: build
    @cd reconciler && cargo build --release --quiet --bin comp-plug
    @./reconciler/target/release/comp-plug {{name}} --wiring

# Compose a component and put the artifact where the showcases expect it.
#
# Every `compose-*` recipe below used to spell out its own `wac plug … --plug …`
# chain: 59 of them, each a hand-maintained list of what an app depends on, none of
# them checked against the app. They were wrong more often than not — `compose-vet`
# named five plugs for a component that imports twenty-two capabilities, and the
# sixteen it omitted were simply left dangling in an artifact `wasm-tools validate`
# accepted (ADR-0087).
#
# Now the list comes from the component. `comp-plug` reads what the artifact
# imports, finds what exports those interfaces, composes each plug before plugging
# it, and keys the result by content. All 49 roots in this file were verified to
# compose this way before the chains were removed.
#
# The output path is unchanged, because the e2e recipes and several tests name it.
_derive name out:
    @cd reconciler && cargo build --release --quiet --bin comp-plug
    @cp "$(./reconciler/target/release/comp-plug {{name}})" {{out}}
    @echo "composed {{name}} (derived from its own imports) -> {{out}}"

compose: build
    @just _derive auth-guard {{guard_composed}}

# Compose the vet-clinic DOMAIN component (the Rust HTTP backend) with every
# capability it imports: the composed auth-guard (auth:identity), records:store,
# validate:schema, search:index. Output is ONE self-contained app component that
# serves HTTP and runs identically on jco or a wasmCloud host — the whole
# vet-clinic backend as language-agnostic wasm, no Node.
compose-vet: compose
    @just _derive vet-domain {{vet_composed}}

# Compose helpdesk-domain (docs/apps/HELPDESK.md rung 1) with every capability it
# imports: the composed auth-guard (auth:identity), records:store,
# fsm:workflow, id:generate, md:render. Remaining imports are generic WASI.
compose-helpdesk: compose
    @just _derive helpdesk-domain {{helpdesk_composed}}

# Compose conduit-domain (docs/apps/CONDUIT.md rung 1 — the RealWorld spec) with the
# capabilities it imports: the composed auth-guard (auth:identity) + records:store.
# Remaining imports are generic WASI. Output is ONE self-contained app component.
compose-conduit: compose
    @just _derive conduit-domain {{conduit_composed}}

# Run the conduit app (docs/apps/CONDUIT.md rung 1) on the native Rust host, in-memory KV.
host-conduit: compose-conduit
    cd host && cargo run --release --bin comp-host -- \
      --app conduit --config-file ../examples/defaults.conf --config default-tenant=conduit \
      --component ../{{conduit_composed}} --addr 0.0.0.0:3008

# conduit e2e: build the composed app + native host, then a Rust test that spawns
# the host and drives the full API (users/profiles/articles/comments/favorites).
e2e-conduit: compose-conduit
    cd host && cargo build --release --bin comp-host
    cd examples/conduit && cargo test --release

# RealWorld conformance (docs/apps/CONDUIT.md rung 4): the OFFICIAL Hurl suite (vendored in
# examples/conduit/conformance/hurl) against the composed app on the native host.
# Requires `hurl` (https://hurl.dev) — like `wash`, not bundled.
conformance-conduit: compose-conduit
    cd host && cargo build --release --bin comp-host
    bash examples/conduit/conformance/run.sh

# Compose saga-domain (docs/apps/SAGA.md — a durable trip-booking saga) with the durable
# primitives it orchestrates: records + fsm + idempotency + event-bus + ids.
# No auth (anonymous engine). Remaining imports are generic WASI.
compose-saga: build
    @just _derive saga-domain {{saga_composed}}

# Run the saga app on the native Rust host. Use --kv nats to prove durability
# (state survives a restart); memory is fine for the happy/compensation paths.
host-saga: compose-saga
    cd host && cargo run --release --bin comp-host -- \
      --app saga --config-file ../examples/defaults.conf --config default-tenant=saga \
      --component ../{{saga_composed}} --addr 0.0.0.0:3012

# Saga e2e: compose + build host + a Rust test that spawns the host and drives
# commit, compensation, and (NATS) resume-after-restart over real HTTP.
e2e-saga: compose-saga
    cd host && cargo build --release --bin comp-host
    cd examples/saga && cargo test --release

# Durability proof (docs/apps/SAGA.md rung 3): start a saga on NATS KV, advance it, KILL
# the host, restart, and show it resumes. Requires NATS on :4222.
durable-saga: compose-saga
    cd host && cargo build --release --bin comp-host
    bash examples/saga/durability.sh

# Golem provider (docs/capabilities/GOLEM.md): unit tests (contract + Value mapping + provider
# compiles). No infra — the live Golem hop skips without GOLEM_E2E.
golem-provider-test:
    cd providers/golem-workflow && cargo test --release

# Live e2e (docs/capabilities/GOLEM.md rung 3): download Golem 1.5, run it, deploy the demo agent,
# and invoke it through the provider's bridge (asserts durable state advances).
golem-e2e:
    bash providers/golem-workflow/e2e.sh

# Live proof (docs/apps/SAGA.md): a saga whose LEGS are real durable Golem workers. Starts
# Golem, deploys the agent, runs the saga with golem-backed legs over wasi:http,
# and asserts it committed with golem-issued refs + the worker's state advanced.
# Requires the Golem binary (run `just golem-e2e` once to fetch it).
saga-golem: compose-saga
    cd host && cargo build --release --bin comp-host
    bash examples/saga/golem-legs.sh

# Compose pulse-domain (docs/apps/REALTIME.md — a realtime chat room with SSE server-push)
# with records + event-bus + id-generate. No auth. Remaining imports are WASI.
compose-pulse: build
    @just _derive pulse-domain {{pulse_composed}}

# Run the chat app on the native Rust host + serve the two-pane SPA. Open two
# browser windows on http://127.0.0.1:3015 and watch messages stream live.
host-pulse: compose-pulse
    cd host && cargo run --release --bin comp-host -- \
      --app pulse --config-file ../examples/defaults.conf --config default-tenant=pulse \
      --component ../{{pulse_composed}} --addr 0.0.0.0:3015 \
      --static-dir ../examples/pulse/public

# Realtime e2e: compose + build host + a Rust test that posts a message and
# proves a SEPARATE held-open SSE connection receives it live.
e2e-pulse: compose-pulse
    cd host && cargo build --release --bin comp-host
    cd examples/pulse && cargo test --release

# Compose jobs-domain (docs/apps/JOBS.md — a durable background-job queue) with its
# capabilities: the outbox (durable queue), the IN-PROCESS durable:workflow
# backend (swap for the golem-workflow provider on a classic host), cron, the
# idempotency guard, and record-store. Remaining imports are WASI.
compose-jobs: build
    @just _derive jobs-domain {{jobs_composed}}

# Compose tempo-domain (docs/apps/TEMPO.md — a multi-person worktime logger) with the
# composed auth-guard (auth:identity) + records. Remaining imports are WASI.
compose-tempo: compose
    @just _derive tempo-domain {{tempo_composed}}

# Run the worktime logger on the native host + serve the SPA. Open
# http://127.0.0.1:3040: register (pick admin to create projects/categories),
# log time or run a pomodoro timer, and see your charts; managers/admins see all.
# Build the React + shadcn SPA (Vite) to examples/tempo/dist.
build-tempo-ui:
    cd examples/tempo/ui && npm ci && npm run build

host-tempo: compose-tempo build-tempo-ui
    cd host && cargo run --release --bin comp-host -- \
      --app tempo --config-file ../examples/defaults.conf --config default-tenant=tempo \
      --component ../{{tempo_composed}} --addr 0.0.0.0:3040 \
      --static-dir ../examples/tempo/dist

# Worktime e2e: compose + build host + a Rust test — admin creates projects +
# categories, members log entries + a pomodoro timer, and the report aggregates
# by project/category over a range with RBAC scope (member=own, manager=all).
e2e-tempo: compose-tempo
    cd host && cargo build --release --bin comp-host
    cd examples/tempo && cargo test --release

# Publish the composed tempo component to GHCR as a public OCI artifact — the
# wasmCloud-native pull path. `gh` mints the token, `wash` does the OCI push.
# One-time setup:
#   gh auth refresh -s write:packages        # add the packages scope to gh
# After the FIRST push, make it public once: GitHub → your profile → Packages →
# tempo → Package settings → Visibility → Public (or "Connect repository").
# Then any wasmCloud host pulls it anonymously:
#   wash start component oci://ghcr.io/{{ghcr_owner}}/tempo:<version> tempo
push-tempo-ghcr version="0.1.0": compose-tempo
    wash oci push ghcr.io/{{ghcr_owner}}/tempo:{{version}} {{tempo_composed}} \
      --user {{ghcr_owner}} --password "$(gh auth token)"
    @echo "pushed oci://ghcr.io/{{ghcr_owner}}/tempo:{{version}} (set the package Public once)"

# Compose booked-domain (docs/apps/BOOKED.md — a Calendly-lite booking service) with the
# composed auth-guard + records + lock-mutex (no double-book) + email-render
# (confirmation) + ical (.ics) + rrule (recurring). Remaining imports are WASI.
compose-booked: compose
    @just _derive booked-domain {{booked_composed}}

# Build the React + shadcn SPA (Vite) to examples/booked/dist.
build-booked-ui:
    cd examples/booked/ui && npm ci && npm run build

# Run the booking app on the native host + serve the SPA on :3041. Register as
# `owner` to create resources + weekly availability; anyone else books free
# slots (no double-book), gets an .ics + a confirmation.
host-booked: compose-booked build-booked-ui
    cd host && cargo run --release --bin comp-host -- \
      --app booked --config-file ../examples/defaults.conf --config default-tenant=booked \
      --component ../{{booked_composed}} --addr 0.0.0.0:3041 \
      --static-dir ../examples/booked/dist

# Booking e2e: owner creates a resource + availability; a member books a slot;
# a SECOND booking of the same slot is rejected (no double-book); concurrent
# attempts leave exactly one booking; a recurrence expands to N instances; and
# a booking exports to a valid .ics.
e2e-booked: compose-booked
    cd host && cargo build --release --bin comp-host
    cd examples/booked && cargo test --release

# Compose transit-domain (docs/apps/TRANSIT.md — a public-transport ticketing service)
# with auth-guard + records (single-use enforced by record-revision CAS) + qr
# (the scannable ticket). Remaining imports are WASI.
compose-transit: compose
    @just _derive transit-domain {{transit_composed}}

# Build the React + shadcn SPA (Vite) to examples/transit/dist.
build-transit-ui:
    cd examples/transit/ui && npm ci && npm run build

# Run the ticketing app on the native host + serve the SPA on :3042. Register as
# `rider` to buy fares (single / 60-min / 90-min / monthly) and show their QR;
# as `validator` to scan + validate with the device camera.
host-transit: compose-transit build-transit-ui
    cd host && cargo run --release --bin comp-host -- \
      --app transit --config-file ../examples/defaults.conf --config default-tenant=transit \
      --component ../{{transit_composed}} --addr 0.0.0.0:3042 \
      --static-dir ../examples/transit/dist

# Ticketing e2e: a rider buys tickets; a validator validates — a single is
# consumed by one scan (a second is rejected); a duration ticket activates with
# a remaining window; CONCURRENT scans of one single ticket accept exactly once;
# a fabricated code is rejected; and a ticket renders a valid QR SVG.
e2e-transit: compose-transit
    cd host && cargo build --release --bin comp-host
    cd examples/transit && cargo test --release

# Compose dashboards-domain (docs/apps/DASHBOARDS.md — personal metric dashboards) with
# auth-guard + records + svg-chart (server-side SVG chart rendering). Remaining
# imports are WASI.
compose-dashboards: compose
    @just _derive dashboards-domain {{dashboards_composed}}

# Build the React + shadcn SPA (Vite) to examples/dashboards/dist.
build-dashboards-ui:
    cd examples/dashboards/ui && npm ci && npm run build

# Run the dashboards app on the native host + serve the SPA on :3043. Register a
# new account (seeded with a demo dashboard); add panels and see them rendered to
# SVG charts on the server — the frontend has no charting library.
host-dashboards: compose-dashboards build-dashboards-ui
    cd host && cargo run --release --bin comp-host -- \
      --app dashboards --config-file ../examples/defaults.conf --config default-tenant=dashboards \
      --component ../{{dashboards_composed}} --addr 0.0.0.0:3043 \
      --static-dir ../examples/dashboards/dist

# Dashboards e2e: a fresh account is seeded a demo dashboard; each panel renders
# to a valid SVG per kind (bar/line/donut/sparkline); a new panel round-trips;
# and one account cannot read another's dashboards.
e2e-dashboards: compose-dashboards
    cd host && cargo build --release --bin comp-host
    cd examples/dashboards && cargo test --release

# Compose gate-domain (docs/apps/GATE.md — a durable traffic-shaping gateway) with records
# (the durable per-key state) + shaper (the token-bucket / GCRA math). The three
# patterns — rate limit, throttle, batch — are the Golem durable-worker model
# expressed over records:store revision CAS. Remaining imports are WASI.
compose-gate: compose
    @just _derive gate-domain {{gate_composed}}

# Build the React + shadcn SPA (Vite) to examples/gate/dist.
build-gate-ui:
    cd examples/gate/ui && npm ci && npm run build

# Run the gateway on the native host + serve the SPA on :3044. Fire bursts at the
# rate limiter (token bucket, 200/429), the throttle (GCRA smoothing), and submit
# items to watch a batch coalesce and flush — all per-key, durable state.
host-gate: compose-gate build-gate-ui
    cd host && cargo run --release --bin comp-host -- \
      --app gate --config-file ../examples/defaults.conf --config default-tenant=gate \
      --component ../{{gate_composed}} --addr 0.0.0.0:3044 \
      --static-dir ../examples/gate/dist

# Gateway e2e: a token bucket allows `capacity` then 429s then refills; GCRA
# admits a burst then spaces with an exact retry-after; concurrent hits on one
# key admit exactly `capacity` (durable per-key CAS = a single-writer worker);
# and a batch coalesces submits and flushes atomically with per-item results.
e2e-gate: compose-gate
    cd host && cargo build --release --bin comp-host
    cd examples/gate && cargo test --release

# Run gate as a REAL Golem agent (docs/apps/GATE.md) and prove EXACT serialization: a
# durable single-writer worker per key admits exactly `capacity` under a
# concurrent burst — where the shared-store gate-domain over-admits. Reuses the
# Golem 1.5 binary from the golem-workflow provider (fetch once via `golem-e2e`).
gate-golem:
    bash examples/gate/golem-run.sh

# Compose books-domain (docs/apps/BOOKS.md — double-entry bookkeeping) with auth-guard +
# records + ledger (the debits==credits invariant + trial balance) + pdf
# (statements). Remaining imports are WASI.
compose-books: compose
    @just _derive books-domain {{books_composed}}

# Build the React + shadcn SPA (Vite) to examples/books/dist.
build-books-ui:
    cd examples/books/ui && npm ci && npm run build

# Run the bookkeeping app on the native host + serve the SPA on :3045. Register
# a new account (seeded a demo chart + entries); post balanced journal entries
# and read the trial balance / P&L / balance sheet (+ PDF).
host-books: compose-books build-books-ui
    cd host && cargo run --release --bin comp-host -- \
      --app books --config-file ../examples/defaults.conf --config default-tenant=books \
      --component ../{{books_composed}} --addr 0.0.0.0:3045 \
      --static-dir ../examples/books/dist

# Bookkeeping e2e: a balanced entry posts; an UNBALANCED entry is rejected; the
# trial balance's debits equal its credits; the balance sheet balances
# (assets = liabilities + equity + net income); and a statements PDF renders.
e2e-books: compose-books
    cd host && cargo build --release --bin comp-host
    cd examples/books && cargo test --release

# Compose stash-domain (docs/apps/STASH.md — a note stash you export as a .zip) with
# auth-guard + records + zip (the archive) + csv (the index inside it). Remaining
# imports are WASI.
compose-stash: compose
    @just _derive stash-domain {{stash_composed}}

# Build the React + shadcn SPA (Vite) to examples/stash/dist.
build-stash-ui:
    cd examples/stash/ui && npm ci && npm run build

# Run the note stash on the native host + serve the SPA on :3046. Register a new
# account (seeded demo notes), keep notes, and hit Export .zip to download them
# all as a real ZIP (Markdown + index.csv + manifest.json).
host-stash: compose-stash build-stash-ui
    cd host && cargo run --release --bin comp-host -- \
      --app stash --config-file ../examples/defaults.conf --config default-tenant=stash \
      --component ../{{stash_composed}} --addr 0.0.0.0:3046 \
      --static-dir ../examples/stash/dist

# Stash e2e: notes CRUD; the export is a valid ZIP (PK header + intact central
# directory) containing a .md per note, an index.csv, and a manifest.json.
e2e-stash: compose-stash
    cd host && cargo build --release --bin comp-host
    cd examples/stash && cargo test --release

# Compose payees-domain (docs/apps/PAYEES.md — a payee book) with auth-guard + records +
# iban (validate the IBAN before storing). Remaining imports are WASI.
compose-payees: compose
    @just _derive payees-domain {{payees_composed}}

# Build the React + shadcn SPA (Vite) to examples/payees/dist.
build-payees-ui:
    cd examples/payees/ui && npm ci && npm run build

# Run the payee book on the native host + serve the SPA on :3047. Register a new
# account (seeded demo payees); add payees — the IBAN is validated as you type
# (country length + mod-97 checksum) and a typo is refused with the reason.
host-payees: compose-payees build-payees-ui
    cd host && cargo run --release --bin comp-host -- \
      --app payees --config-file ../examples/defaults.conf --config default-tenant=payees \
      --component ../{{payees_composed}} --addr 0.0.0.0:3047 \
      --static-dir ../examples/payees/dist

# Payee-book e2e: a valid IBAN is accepted (stored normalized + country); a
# bad-checksum / wrong-length / bad-country IBAN is rejected with the reason;
# /verify returns the parsed country + grouped form; and ownership is enforced.
e2e-payees: compose-payees
    cd host && cargo build --release --bin comp-host
    cd examples/payees && cargo test --release

# Compose lms-domain (docs/apps/LMS.md — a learning platform) with auth-guard + records +
# quiz (auto-grade + stats) + pdf (certificate) + svg-chart (gradebook chart).
# Remaining imports are WASI.
compose-lms: compose
    @just _derive lms-domain {{lms_composed}}

# Build the React + shadcn SPA (Vite) to examples/lms/dist.
build-lms-ui:
    cd examples/lms/ui && npm ci && npm run build

# Run the learning platform on the native host + serve the SPA on :3048. Register
# as `instructor` (creates courses/lessons/quizzes; seeded a demo course) or as
# `student` (enroll, take auto-graded quizzes, see progress + certificate).
host-lms: compose-lms build-lms-ui
    cd host && cargo run --release --bin comp-host -- \
      --app lms --config-file ../examples/defaults.conf --config default-tenant=lms \
      --component ../{{lms_composed}} --addr 0.0.0.0:3048 \
      --static-dir ../examples/lms/dist

# Learning e2e: an instructor creates a course + quiz; a student enrolls and
# submits, which auto-grades (quiz:grade); the instructor gradebook reflects it
# consistently; a certificate issues once every quiz is passed; and the student's
# progress reconciles with the gradebook.
e2e-lms: compose-lms
    cd host && cargo build --release --bin comp-host
    cd examples/lms && cargo test --release

# Compose buzz-domain (docs/apps/BUZZ.md — a live multiplayer quiz game) with auth-guard +
# records. Remaining imports are WASI (random for the PIN, clocks for timing).
compose-buzz: compose
    @just _derive buzz-domain {{buzz_composed}}

# Build the React + shadcn SPA (Vite) to examples/buzz/dist.
build-buzz-ui:
    cd examples/buzz/ui && npm ci && npm run build

# Run the quiz game on the native host + serve the SPA on :3049. Sign in as a
# host to run a game (get a PIN), or open on other devices to Join with the PIN
# and a nickname; the host drives the questions and everyone buzzes in.
host-buzz: compose-buzz build-buzz-ui
    cd host && cargo run --release --bin comp-host -- \
      --app buzz --config-file ../examples/defaults.conf --config default-tenant=buzz \
      --component ../{{buzz_composed}} --addr 0.0.0.0:3049 \
      --static-dir ../examples/buzz/dist

# Game e2e: a host starts a game; two players join and answer at different speeds;
# reveal grades speed-weighted (faster-correct > slower-correct > wrong=0); the
# leaderboard ranks correctly; and the game ends on a podium.
e2e-buzz: compose-buzz
    cd host && cargo build --release --bin comp-host
    cd examples/buzz && cargo test --release

# Compose photosocial-domain (docs/apps/PHOTOSOCIAL.md — social photo sharing with AI critique & RBAC attributes)
compose-photosocial: compose
    @just _derive photosocial-domain {{photosocial_composed}}

host-photosocial: compose-photosocial
    cd host && cargo run --release --bin comp-host -- \
      --app photosocial --config-file ../examples/defaults.conf --config default-tenant=photosocial \
      --component ../{{photosocial_composed}} --addr 0.0.0.0:3055

e2e-photosocial: compose-photosocial
    cd host && cargo build --release --bin comp-host
    cd examples/photosocial && cargo test --release

screencast-photosocial: compose-photosocial
    node tools/screencast/photosocial.mjs
    bash tools/screencast/to-gif.sh tools/screencast/videos/photosocial/*.webm docs/media/photosocial.gif 820 10

# Compose mesh-domain (docs/apps/MESH.md — resilient upstream calls) with records (the
# durable per-key circuit state) + resilience (the breaker state machine and the
# backoff schedule) + proxy-route (the REAL outgoing HTTP hop). Remaining imports
# are WASI: clocks for latency + the backoff sleep, config for the route table.
compose-mesh: compose
    @just _derive mesh-domain {{mesh_composed}}

# Build the React + shadcn SPA (Vite) to examples/mesh/dist.
build-mesh-ui:
    cd examples/mesh/ui && npm ci && npm run build

# The deliberately flaky upstream mesh protects callers from (std-only, ~100
# lines). Fails on demand per request: /hit?fail=1, ?fail_n=2&id=x, ?delay=400.
# `host-mesh` starts it for you; run it alone to keep it up across host restarts.
mesh-upstream:
    cd examples/mesh && cargo run --release --bin flaky -- 127.0.0.1:3051

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

# Resilience e2e against the REAL flaky upstream: retries ride out a two-request
# blip; `failure_threshold` failures trip the breaker and while it is OPEN the
# upstream's own hit counter proves it is never dialled; a half-open probe closes
# it again; a response slower than `slo_ms` counts as failed despite its 200; an
# unreachable upstream trips the breaker but a missing route (our config bug)
# does not.
e2e-mesh: compose-mesh
    cd host && cargo build --release --bin comp-host
    cd examples/mesh && cargo build --release --bin flaky && cargo test --release

# Compose passkey-domain (docs/apps/PASSKEY.md — passwordless WebAuthn sign-in) with
# webauthn (the ceremony verification: CBOR/COSE + ES256/RS256 signatures) +
# records (accounts + credentials) + cache (single-use challenges with a TTL) +
# session-store (the session a completed ceremony mints). Remaining imports are
# WASI: random for challenges, clocks, and config for the RP id + origin.
compose-passkey: build
    @just _derive cache components/target/cache.composed.wasm
    @just _derive passkey-domain {{passkey_composed}}

# Build the React + shadcn SPA (Vite) to examples/passkey/dist.
build-passkey-ui:
    cd examples/passkey/ui && npm ci && npm run build

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

# Passkey e2e with a VIRTUAL AUTHENTICATOR: the test holds a P-256 key and
# performs the real ceremonies over HTTP (CBOR attestation object, COSE key,
# DER-encoded ECDSA over authData || sha256(clientDataJSON)). It registers, logs
# in, and then proves each check bites: a replayed challenge, a phishing origin,
# a credential from another RP, a signature from the wrong key, and a counter
# that went backwards are all refused — by reason.
e2e-passkey: compose-passkey
    cd host && cargo build --release --bin comp-host
    cd examples/passkey && cargo test --release

# Compose studio-domain (docs/apps/STUDIO.md — the composition studio) with wit-reflect
# (inspection + wac's own composition engine) + records (surfaces + saved
# canvases) + blob-store (the uploaded component bytes). Remaining imports are
# WASI. Note wit_reflect.wasm is ~1 MB: it carries wasmparser and wac-graph, so
# the studio can compose for real instead of printing instructions.
compose-studio: build
    @just _derive studio-domain {{studio_composed}}

# Build the React + xyflow SPA (Vite) to examples/studio/dist.
build-studio-ui:
    cd examples/studio/ui && npm ci && npm run build

# Feed the studio every component in this repo, by POSTing the actual .wasm
# artifacts — a component cannot read the filesystem (the host preopens no
# directories), so reflection has to be fed over HTTP. Re-running is safe: an
# upload with the same id replaces it. Needs `just host-studio` already running.
seed-studio addr="127.0.0.1:3054":
    #!/usr/bin/env bash
    set -euo pipefail
    ok=0; skipped=0
    for f in {{rel}}/*.wasm; do
      name=$(basename "$f" .wasm | tr '_' '-')
      code=$(curl -s -o /dev/null -w '%{http_code}' -X POST --data-binary "@$f" \
        -H 'content-type: application/wasm' "http://{{addr}}/api/components?id=$name" || echo 000)
      if [ "$code" = "201" ]; then ok=$((ok+1)); else skipped=$((skipped+1)); echo "  $code $name"; fi
    done
    echo "seeded $ok components ($skipped not accepted)"

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

# Compose platform-domain (docs/adr/ — the multi-tenant deployment platform) with
# the composed auth-guard (accounts/sessions/RBAC) + policy-guard (ownership and
# visibility as rules) + records (tenants/deployments/revisions) + blob (staged
# uploads) + quota (per-tenant budgets) + wit-reflect (inspect/plan/compose).
# Remaining imports are WASI. Note it no longer needs outgoing-handler for anything:
# with the applier gone (ADR-0022) the control plane makes no outbound calls at all,
# so it runs with egress denied.
compose-platform: compose
    @just _derive platform-domain {{platform_composed}}

# Build the native reconciler — the only process holding a lattice credential.
build-reconciler:
    cd reconciler && cargo build --release

# The whole platform locally: NATS, the control plane on :8080, the reconciler, and
# one lattice node on :3401. Then drive it with `./cli/target/release/comp`:
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

# Stop every local platform process. Safe to run at any time, and safe when
# nothing is running.
#
# It no longer deletes namespaces, because there are none (ADR-0021). Instances on
# a lattice node are stopped by deleting the deployment — `comp app rm` — which the
# reconciler then converges; killing a host here only stops this box.
platform-teardown:
    #!/usr/bin/env bash
    set -uo pipefail
    echo "-- local processes:"
    pkill -f 'comp-host' 2>/dev/null && echo "   stopped comp-host" || echo "   no comp-host running"
    pkill -f 'comp-reconciler' 2>/dev/null && echo "   stopped comp-reconciler" || echo "   no reconciler running"
    pkill -f 'nats-server -js -sd' 2>/dev/null && echo "   stopped nats-server" || echo "   no local nats running"
    docker rm -f comp-registry >/dev/null 2>&1 && echo "   removed the local comp-registry container" || true
    echo "-- left behind ON PURPOSE: artifacts in a JetStream object store are"
    echo "   content-addressed, so they are cheap to keep and safe to re-push."

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

# Build ONE self-contained image (comp-host + composed component + built SPA).
# No wasmCloud — comp-host serves http + the SPA + Redis-backed storage in one
# process. Run: docker run -p 8080:8080 -e REDIS_URL=rediss://... tempo
docker-tempo: compose-tempo build-tempo-ui
    docker build -f examples/tempo/Dockerfile -t tempo .
    @echo "built image 'tempo' — docker run -p 8080:8080 -e REDIS_URL=rediss://user:pw@host:25061 tempo"

# Compose arena-domain (docs/apps/ARENA.md — multiplayer Connect Four) with records +
# id-generate. Remaining imports are WASI.
compose-arena: build
    @just _derive arena-domain {{arena_composed}}

# Run the game on the native host + serve the SPA. Open two windows on
# http://127.0.0.1:3039 — create a game in one, join from the other, play live.
host-arena: compose-arena
    cd host && cargo run --release --bin comp-host -- \
      --config registry=registry.platform.svc.cluster.local:5000 --config cluster-suffix=svc.cluster.local --config registry="$REG" --config cluster-suffix=svc.cluster.local \
      --app arena --config-file ../examples/defaults.conf --config default-tenant=arena \
      --component ../{{arena_composed}} --addr 0.0.0.0:3039 \
      --static-dir ../examples/arena/public

# Game e2e: compose + build host + a Rust test that plays a full game — create,
# join, turn/seat/illegal-move rejection, a scripted win, and a concurrent-move
# revision conflict.
e2e-arena: compose-arena
    cd host && cargo build --release --bin comp-host
    cd examples/arena && cargo test --release

# Golem-backed variant: same queue, but durable:workflow is satisfied by the
# golem-bridge component (calls a durable Golem worker over wasi:http) instead of
# the in-process backend. The composed wasm now imports wasi:http/outgoing-handler
# (a host interface on the v2 operator). Point it at Golem via CFG_GOLEM_URL.
compose-jobs-golem: build
    @just _derive jobs-domain components/target/jobs_domain.golem.wasm

jobs_reg := env_var_or_default("JOBS_REG", "localhost:30501")

# Run the job queue on the native host + serve the board SPA. Open
# http://127.0.0.1:3038: enqueue jobs, watch them run/retry/dead-letter live,
# replay from the DLQ. CFG tunes the outbox: 3 attempts, 1s base backoff.
host-jobs: compose-jobs
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=jobs --config max-attempts=2 --config base-backoff=1 \
      --component ../{{jobs_composed}} --addr 0.0.0.0:3038 \
      --static-dir ../examples/jobs/public

# Job-queue e2e: compose + build host + a Rust test that enqueues jobs, drives
# ticks, and proves success / retry-then-succeed / dead-letter / replay + the
# exactly-once enqueue key.
e2e-jobs: compose-jobs
    cd host && cargo build --release --bin comp-host
    cd examples/jobs && cargo test --release

# Compose scribe-domain (docs/apps/SCRIBE.md — a collaborative document editor) with the
# crdt merge component + records + id-generate. Remaining imports are WASI.
compose-scribe: build
    @just _derive scribe-domain {{scribe_composed}}

# Run the collaborative editor on the native host + serve the two-pane SPA. Open
# two windows on http://127.0.0.1:3037 and edit the same doc — edits merge and
# stream live to both.
host-scribe: compose-scribe
    cd host && cargo run --release --bin comp-host -- \
      --app scribe --config-file ../examples/defaults.conf --config default-tenant=scribe \
      --component ../{{scribe_composed}} --addr 0.0.0.0:3037 \
      --static-dir ../examples/scribe/public

# Collaborative-editor e2e: compose + build host + a Rust test proving two
# concurrent edits merge (both survive) and a live SSE connection sees them.
e2e-scribe: compose-scribe
    cd host && cargo build --release --bin comp-host
    cd examples/scribe && cargo test --release

# Compose pipeline-domain (docs/apps/PIPELINE.md — a reliable event pipeline with
# outbox → dispatch → DLQ → replay, SSE server-push) with outbox + event-bus +
# id-generate. No auth. Remaining imports are WASI (bound at deploy).
compose-pipeline: build
    @just _derive pipeline-domain {{pipeline_composed}}

# Run the pipeline board on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3016: POST events, toggle the sink down, watch retries drop
# to the dead-letter tray, then Replay them — live over SSE.
host-pipeline: compose-pipeline
    cd host && cargo run --release --bin comp-host -- \
      --app pipeline --config-file ../examples/defaults.conf --config default-tenant=pipeline \
      --component ../{{pipeline_composed}} --addr 0.0.0.0:3016 \
      --static-dir ../examples/pipeline/public

# Reliability e2e: compose + build host + a Rust test that enqueues events,
# proves they deliver (acked), then takes the sink down and proves an event
# retries and drops to the dead-letter tray, and that Replay requeues it.
e2e-pipeline: compose-pipeline
    cd host && cargo build --release --bin comp-host
    cd examples/pipeline && cargo test --release

# Compose flags-domain (docs/apps/FLAGS.md — a live feature-rollout console with SSE
# server-push) with feature-flags + event-bus + id-generate. No auth. Remaining
# imports are WASI (kv + config bound at deploy).
compose-flags: build
    @just _derive flags-domain {{flags_composed}}

# Run the rollout console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3017: drag a flag to 30% and watch ~30 of 100 subject tiles
# light up instantly and stay sticky; trip the kill-switch — all dark at once.
host-flags: compose-flags
    cd host && cargo run --release --bin comp-host -- \
      --app flags --config-file ../examples/defaults.conf --config default-tenant=flags \
      --component ../{{flags_composed}} --addr 0.0.0.0:3017 \
      --static-dir ../examples/flags/public

# Rollout e2e: compose + build host + a Rust test that sets a 30% rule and
# proves (a) a subject is STICKY across repeated evals, (b) raising the
# percentage never turns an already-on subject off, and (c) a rule flip made by
# one request reaches a SEPARATE held-open SSE connection live.
e2e-flags: compose-flags
    cd host && cargo build --release --bin comp-host
    cd examples/flags && cargo test --release

# Compose abtest-domain (docs/apps/EXPERIMENT.md — an A/B/n experiment console with SSE)
# with experiment-assign + metrics-collect + event-bus + id-generate. No auth.
# Remaining imports are WASI (kv + config bound at deploy).
compose-abtest: build
    @just _derive abtest-domain {{abtest_composed}}

# Run the experiment console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3018: define control/variant-a/variant-b weights, watch 100
# subjects split into arms (sticky as weights shift), fire conversions, and see
# the per-arm conversion-rate bars pull apart live.
host-abtest: compose-abtest
    cd host && cargo run --release --bin comp-host -- \
      --app abtest --config-file ../examples/defaults.conf --config default-tenant=abtest \
      --component ../{{abtest_composed}} --addr 0.0.0.0:3018 \
      --static-dir ../examples/abtest/public

# Experiment e2e: compose + build host + a Rust test that defines a 50/25/25
# experiment and proves (a) assignment is STICKY per subject, (b) two different
# subjects can land in different arms, (c) the ~50/25/25 split holds across a
# cohort, (d) conversions attribute to the right arm's rate, and (e) an outcome
# recorded by one request reaches a SEPARATE held-open SSE connection live.
e2e-abtest: compose-abtest
    cd host && cargo build --release --bin comp-host
    cd examples/abtest && cargo test --release

# Compose search-domain (docs/apps/SEARCH.md — faceted search-as-you-type) with the
# engine + corpus + cache (pre-composed with its kv backing) + metrics +
# pagination + ids. No auth. Remaining imports are WASI (kv + config).
compose-search: build
    @just _derive cache components/target/cache.composed.wasm
    @just _derive search-domain {{search_composed}}

# Run the search console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3019: type in the box, watch ranked hits narrow live, click a
# facet chip to filter, and watch the cache hit-ratio climb on repeat queries.
host-search: compose-search
    cd host && cargo run --release --bin comp-host -- \
      --app search --config-file ../examples/defaults.conf --config default-tenant=search \
      --component ../{{search_composed}} --addr 0.0.0.0:3019 \
      --static-dir ../examples/search/public

# Search e2e: compose + build host + a Rust test that seeds the corpus and
# proves ranked results (a rare term ranks its doc first), all-mode intersection
# shrinks the set, a tag facet restricts hits, and the cache serves a repeat
# query (hit-ratio rises).
e2e-search: compose-search
    cd host && cargo build --release --bin comp-host
    cd examples/search && cargo test --release

# Compose throttle-domain (docs/apps/RATELIMIT.md — a live throttle wall) with the two
# limiters + event-bus + id-generate. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-ratelimit: build
    @just _derive throttle-domain {{throttle_composed}}

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

# Throttle e2e: compose + build host + a Rust test that proves N allowed then a
# 429 at the ceiling, a quota `remaining` that decrements, lockout after a burst
# of failures, and a verdict reaching a SEPARATE held-open SSE connection.
e2e-ratelimit: compose-ratelimit
    cd host && cargo build --release --bin comp-host
    cd examples/ratelimit && cargo test --release

# Compose upload-drop (docs/apps/DROP.md — a presigned direct-upload drop-box) with the
# gate + blob store + signer + records + ids. No auth. Remaining imports are
# WASI (kv + config bound at deploy — see CFG_* below).
compose-drop: build
    @just _derive upload-drop {{drop_composed}}

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

# Drop e2e: compose + build host + a Rust test that proves a ticket is minted
# for an allowed type, an oversized/blocked type is rejected at ticket time, a
# redeemed ticket stores bytes, and a signed download link round-trips the bytes
# while a tampered signature is refused.
e2e-drop: compose-drop
    cd host && cargo build --release --bin comp-host
    cd examples/drop && cargo test --release

# Compose csv-report (docs/apps/REPORT.md — batch CSV import/report) with the codec +
# validator + records + pagination. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-report: build
    @just _derive csv-report {{report_composed}}

# Run the CSV import/report tool on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3022: paste a CSV, watch valid rows import and bad rows come
# back with per-field errors, page the clean report, then export it back to CSV.
host-report: compose-report
    cd host && \
      cargo run --release --bin comp-host -- \
        --config default-tenant=report \
      --component ../{{report_composed}} --addr 0.0.0.0:3022 \
      --static-dir ../examples/report/public

# Report e2e: compose + build host + a Rust test that imports a CSV with a mix
# of valid + invalid rows (proving typed validation splits them with per-field
# errors), pages the clean set through the opaque cursor, and exports it back to
# CSV through the same codec (round-trip).
e2e-report: compose-report
    cd host && cargo build --release --bin comp-host
    cd examples/report && cargo test --release

# Compose mfa-authgate (docs/apps/AUTHGATE.md — TOTP 2FA + challenge-response login) with
# the otp primitive + secrets vault + session store + records. No auth-guard —
# this app IS the second factor. secrets:vault needs a 32-byte base64 master-key
# from config (CFG_MASTER_KEY below).
compose-authgate: build
    @just _derive mfa-authgate {{authgate_composed}}

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
compose-paste: build
    @just _derive paste-bin {{paste_composed}}

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

# Paste e2e: compose + build host + a Rust test that proves an empty body is
# rejected (validate), PII in the body is masked BEFORE storage (the raw email
# never appears in the stored/raw output), Markdown renders to sanitized HTML
# (a <script> is escaped, not executed), and duplicate titles get distinct
# slugs.
e2e-paste: compose-paste
    cd host && cargo build --release --bin comp-host
    cd examples/paste && cargo test --release

# Build the track SPA (Vite + TS) into components/track-assets/static, so the
# track-assets component's build.rs embeds it. Run before compose-track.
build-track-ui:
    cd examples/track/ui && npm install && npm run build

# Compose track-domain (docs/apps/TRACK.md — a Linear-lite project tracker) — the biggest
# composition in the repo: the pre-composed auth-guard + records + fsm + search +
# event-bus + notify + webhook-sign + policy + paginate + markdown + the
# pre-composed ai-inference (mock llm) + the baked SPA (track-assets). Five axes
# in one self-contained component. Depends on `compose` (guard), `compose-ai`
# (ai+mock-llm), and the built SPA.
compose-track: build-track-ui compose compose-ai
    @just _derive track-domain {{track_composed}}

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

# Track e2e: compose + build host + a Rust test driving all five axes — auth +
# RBAC (admin creates a project, a member writes, a non-member is 403), issue
# lifecycle over the fsm, full-text search, an SSE activity frame, the background
# The `bytes:codec` specification, run against the ARTIFACT rather than the crate.
#
# `components/bytes-codec/tests/codec.rs` calls the Rust functions directly. That is a
# fine unit test and it cannot judge a component built in another language, or one
# fetched by digest and never built here at all. This drives the same cases through
# `codec-probe` over HTTP, so what is judged is whatever satisfies the contract.
#
# That is the precondition for both polyglot components and prebuilt artifacts: a gate
# at the WIT boundary does not care what compiled the thing it is judging.
gate-codec:
    cd host && cargo build --release --bin comp-host
    cd reconciler && cargo build --release --bin comp-plug
    cd components && cargo build --release --target wasm32-wasip2 -p bytes-codec -p codec-probe
    bash components/bytes-codec/gate.sh

# Fetch the built components instead of building them.
#
# `components/target` is 7.1 GB of intermediates for 25 MB of output — a 284:1 ratio
# that has to be paid on every machine, for artifacts CI already produced from this
# same tree with this same `just build`.
#
# So: pull them. By default for the commit you are on; pass a ref to take another's.
# Anything you are actually editing still rebuilds, because cargo compares mtimes and
# these arrive newer than the sources only if the sources have not changed since.
#
#   just fetch-components              # this commit
#   just fetch-components main         # whatever main last built
#
# Needs `gh` and a successful run for that commit. It refuses rather than silently
# fetching a different tree's bytes: a component that does not match your source is
# the worst possible thing to debug.
fetch-components ref="":
    #!/usr/bin/env bash
    set -euo pipefail
    want="{{ref}}"
    [ -n "$want" ] || want="$(git rev-parse HEAD)"
    echo "looking for a components build of $want …"
    run=$(gh run list --workflow ci.yml --commit "$(git rev-parse "$want")" \
            --status success --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)
    if [ -z "$run" ]; then
      echo "no successful CI run for $want." >&2
      echo "  push it, wait for the 'components (wasm32-wasip2)' job, or 'just build' locally." >&2
      exit 1
    fi
    out=components/target/wasm32-wasip2/release
    mkdir -p "$out"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    if ! gh run download "$run" --name components-wasm32-wasip2 --dir "$tmp" 2>/dev/null; then
      echo "run $run has no components artifact." >&2
      echo "  Runs from before the 'Keep the artifacts' step do not carry one, and an" >&2
      echo "  artifact expires after 30 days. Push again, or 'just build'." >&2
      exit 1
    fi
    n=$(find "$tmp" -name '*.wasm' | wc -l | tr -d ' ')
    [ "$n" -gt 0 ] || { echo "the artifact held no .wasm files" >&2; exit 1; }
    cp "$tmp"/*.wasm "$out/"
    echo "fetched $n component(s) from run $run into $out"
    echo "  they are the bytes that run tested; `just build` still rebuilds anything you edit."

# stale-sweep tick, and the AI thread summary.
e2e-track: compose-track
    cd host && cargo build --release --bin comp-host
    cd examples/track && cargo test --release

# FULL-PARITY compose: plug every capability the parity vet-domain imports into
# one app component — all 19 (auth-guard, records, validate, search, blob,
# upload, fsm, money, markdown, csv, pii, otp, secrets, i18n, pagination,
# ai-inference (+mock llm, pre-composed), cache, timer, lock, event-bus). Output
# is the whole feature-complete vet-clinic backend as ONE wasm.
compose-vet-full: compose compose-ai
    # cache needs a backing store (source/sink); pre-compose cache + cache-backing
    # so the pair has zero non-WASI imports, then plug the pair.
    @just _derive cache components/target/cache.composed.wasm
    @just _derive vet-domain {{vet_full_composed}}

# LATTICE compose (wasmCloud): fuse ONLY the pure-compute capabilities into
# vet-domain — each is ~4 core modules, and wasmtime caps a component at 30
# nested core-module instances (the fused-everything artifact is 104 and does
# not deploy). vet-domain + these 6 = 28 modules; csv (admin export, coldest
# path) stays linked to fit. Every stateful/swap-point capability (auth,
# records, search, blob, fsm, otp, secrets, i18n, ai, cache, timer, lock,
# event-bus, ui/static, csv) remains a wadm LINK. This removes the per-call
# wrpc-over-NATS hop for pure compute while keeping the lattice where it earns
# its cost (durability, scaling, hot-swap). LATTICE=1 gen-manifest.py drops the
# fused capabilities from the manifest.
compose-vet-lattice: build
    @just _derive vet-domain {{vet_lattice}}

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

# Compose the eshop-catalog service (docs/apps/ESHOP.md): eShopOnDapr's Catalog.API over
# record-store + event-bus + idempotency-guard (at-least-once dedup for the
# stock consumers). Output imports only generic WASI.
compose-eshop-catalog: build
    @just _derive eshop-catalog {{eshopcatalog_composed}}

# Compose every eshop service (docs/apps/ESHOP.md): eShopOnDapr recreated over comp
# contracts. identity = the existing accounts-app + composed auth-guard,
# untouched. Each output imports only generic WASI.
compose-eshop: compose compose-eshop-catalog
    @just _derive eshop-basket {{eshopbasket_composed}}
    @just _derive eshop-ordering {{eshopordering_composed}}
    @just _derive eshop-payment {{eshoppayment_composed}}
    @just _derive accounts-app {{eshopidentity_composed}}
    @just _derive eshop-gateway {{eshopgateway_composed}}
    @just _derive event-pusher components/target/event_pusher.composed.wasm

# Run the whole eshop (identity/catalog/basket/ordering/payment + gateway with
# the embedded storefront) on native hosts over a shared NATS at :4222.
# Gateway/storefront: http://127.0.0.1:3100 — smoke: examples/eshop/smoke.sh
host-eshop: compose-eshop
    examples/eshop/run-local.sh

eshop_reg := env_var_or_default("ESHOP_REG", "localhost:30500")

# Compose the idempotency-guard into webhook-ingest, satisfying its
# `idempotency:guard/store` import. Demonstrates one component composing another.
compose-webhook: build
    @just _derive webhook-ingest {{webhook_composed}}

# Compose THREE capabilities — session:store + config:store + secrets:vault —
# into the login-app consumer, satisfying all three of its imports at once.
# The multi-capability composition demo: the output imports nothing but generic
# WASI host shims.
compose-login: build
    @just _derive login-app {{login_composed}}

# Compose the link-shortener app: slug + id-generate + record-store +
# rate-limiter + cache (pre-composed with its kv backing). Output imports only
# generic WASI (keyvalue/clocks/random/config), so any comp host runs it.
compose-shortlink: build
    @just _derive cache components/target/cache.composed.wasm
    @just _derive link-shortener {{shortlink_composed}}

# Run the composed link-shortener under the native host.
host-shortlink: compose-shortlink
    cd host && cargo run --release --bin comp-host -- --component ../{{shortlink_composed}} --addr 127.0.0.1:3008

# Compose the dev-portal app: the composed auth-guard (auth:identity) +
# record-store + id-generate + quota + policy-guard + outbox + webhook-sign +
# notify-dispatch. RBAC gates role verbs, policy-guard gates project access;
# key events leave as stripe-signed webhooks on an admin-pumped outbox drain.
compose-portal: compose
    @just _derive dev-portal {{portal_composed}}

# Run the composed dev-portal under the native host.
host-portal: compose-portal
    cd host && cargo run --release --bin comp-host -- --component ../{{portal_composed}} --addr 127.0.0.1:3009

# Compose the webhook-relay app: the composed webhook-ingest (HMAC verify +
# replay dedup) + jsonpatch + outbox + webhook-sign + notify-dispatch +
# rate-limiter + audit-log + record-store. Ingest -> transform -> durable
# queue; drain delivers github-signed webhooks with retry + dead letters.
compose-relay: compose-webhook
    @just _derive webhook-relay {{relay_composed}}

# Run the composed webhook-relay under the native host.
host-relay: compose-relay
    cd host && cargo run --release --bin comp-host -- --component ../{{relay_composed}} --addr 127.0.0.1:3010

# Compose the billing-ledger app: money + record-store + idempotency-guard +
# quota + csv + outbox. Idempotency-key replay cache on the write path,
# integer minor-unit arithmetic, revision-CAS balances, csv statements.
compose-ledger: build
    @just _derive billing-ledger {{ledger_composed}}

# Run the composed billing-ledger under the native host.
host-ledger: compose-ledger
    cd host && cargo run --release --bin comp-host -- --component ../{{ledger_composed}} --addr 127.0.0.1:3011

# Compose the status-page app: scheduler-timer + record-store + fsm-workflow +
# event-bus + notify-dispatch. Timer-driven probes over outgoing HTTP; state
# transitions fan out on the bus and alert as webhooks.
compose-status: build
    @just _derive status-page {{statuspage_composed}}

# Run the composed status-page under the native host. Open
# http://127.0.0.1:3012: add a monitor (url + period >= 10s), then POST
# /api/tick to probe — the page shows each monitor's up/degraded/down state and
# its fsm transition history (up -> degraded -> down needs TWO failures).
host-status: compose-status
    cd host && cargo run --release --bin comp-host -- --component ../{{statuspage_composed}} --addr 127.0.0.1:3012

# Status-page e2e: compose + build host + a Rust test that adds a self-probe
# (stays up) and a dead-port monitor, then proves the fsm walks up -> degraded
# (one failure) -> down (a second consecutive failure), with both transitions in
# the history log. Slow: monitors have a 10s minimum period, so it sleeps across
# a period to force the second probe.
e2e-status: compose-status
    cd host && cargo build --release --bin comp-host
    cd examples/status && cargo test --release

# Compose an LLM provider into the ai-inference domain layer, satisfying its
# `llm:inference/inference` import. Here the deterministic MOCK provider is
# plugged in (for offline tests + demo); swap --plug for a real provider
# component (openai/anthropic/ollama) to go live — ai-inference is unchanged.
compose-ai: build
    @just _derive ai-inference components/target/llm_local.composed.wasm

# Same domain layer, but with the REAL openai-provider plugged in instead of the
# mock — proves the swap is a composition choice, not a code change. The provider
# imports wasi:http + wasi:config (base-url / api-key / model), which the host
# (wasmCloud httpclient + config, or a jco http shim) satisfies at runtime.
compose-ai-openai: build
    @just _derive ai-inference components/target/llm_local.openai.composed.wasm

# Same composition, the DECLARATIVE way — `wac compose` over a .wac source file
# (components/login-app/compose.wac) instead of the imperative `wac plug` chain
# above. The .wac file states the wiring explicitly. Output is equivalent.
compose-login-wac: build
    wac compose {{components}}/login-app/compose.wac \
        --dep login:component={{loginapp_wasm}} \
        --dep session:store={{session_wasm}} \
        --dep config:store={{config_wasm}} \
        --dep secrets:vault={{secrets_wasm}} \
        -o components/target/login_app.wac-composed.wasm

# Validate the built components.
validate: build
    @echo "both components valid"

# Show the world each built component imports/exports.
inspect: build
    @echo "=== auth-guard ===" && wasm-tools component wit {{guard_wasm}} | grep -E "import|export"
    @echo "=== sample-consumer ===" && wasm-tools component wit {{consumer_wasm}} | grep -E "import|export"

# Bring up NATS + the Zitadel IdP profile.
up-zitadel:
    docker compose -f infra/compose.yaml --profile zitadel up -d

# Bring up NATS + the Ory IdP profile.
up-ory:
    docker compose -f infra/compose.yaml --profile ory up -d

# Tear everything down.
down:
    docker compose -f infra/compose.yaml --profile zitadel --profile ory down -v

# Deploy on wasmCloud 1.x via wadm/OAM (needs `wash`). `wash up` first.
deploy: build
    wash app put infra/wadm.yaml
    wash app deploy comp-auth

# Deploy on the wasmCloud k8s operator: apply host + OAM app, then collapse to a
# single lattice host. Needs kubectl + the operator + components pushed to the
# in-cluster registry (see README). `ns` defaults to comp-auth.
ns := "comp-auth"
# Full local check: vendor (if needed), validate WIT, build, validate components.
check: wit-check validate
    @echo "OK — contract resolves and both components build clean"

# ---------------------------------------------------------------------------
# Self-hosting lane (tier 1 of docs/SELFHOST.md): comp-host + systemd + a
# per-app URL. No Kubernetes, no operator, no NATS.
# ---------------------------------------------------------------------------

build-selfhost:
    cd cli && cargo build --release

# Cross-build a STATIC comp-host for a Linux box.
#
# musl on purpose: the result has no glibc version to match, so one binary runs on
# Debian, Ubuntu or Alpine. Needs `cross` and a running docker/orbstack. Takes ~4
# minutes cold, because wasmtime.
#
#   just selfhost-build-host              # x86_64 (most VPSes)
#   just selfhost-build-host aarch64      # ARM boxes, Pi, Hetzner/Oracle ARM
selfhost-build-host arch="x86_64":
    cd host && cross build --release --target {{arch}}-unknown-linux-musl
    @file host/target/{{arch}}-unknown-linux-musl/release/comp-host
    @ls -la host/target/{{arch}}-unknown-linux-musl/release/comp-host | awk '{printf "  %.0f MB\n", $5/1048576}'

# Cross-build a STATIC comp-relay — what makes a pull-based timer or topic fire.
#
# Small next to comp-host: it is an HTTP client with a clock and JITs nothing.
selfhost-build-relay arch="x86_64":
    cd reconciler && cross build --release --target {{arch}}-unknown-linux-musl --bin comp-relay
    @ls -la reconciler/target/{{arch}}-unknown-linux-musl/release/comp-relay | awk '{printf "  comp-relay %.0f MB\n", $5/1048576}'

# ONE-TIME per box. Installs the runtime and makes Caddy read what this lane writes.
#
# Without this, `selfhost-deploy` would install a unit pointing at a comp-host that
# does not exist, and drop site files into a directory Caddy never reads — so it would
# look like it worked and serve nothing.
selfhost-bootstrap host arch="x86_64": (selfhost-build-host arch) (selfhost-build-relay arch)
    #!/usr/bin/env bash
    set -euo pipefail
    BIN=host/target/{{arch}}-unknown-linux-musl/release/comp-host
    scp "$BIN" {{host}}:/tmp/comp-host
    # comp-relay too: an app whose spec declares [triggers] gets a second unit, and a
    # unit pointing at a binary that is not there fails at start rather than at deploy.
    RELAY=reconciler/target/{{arch}}-unknown-linux-musl/release/comp-relay
    if [ -f "$RELAY" ]; then scp "$RELAY" {{host}}:/tmp/comp-relay; fi
    ssh {{host}} "set -e; \
      sudo install -m 0755 /tmp/comp-host /usr/local/bin/comp-host; rm -f /tmp/comp-host; \
      if [ -f /tmp/comp-relay ]; then sudo install -m 0755 /tmp/comp-relay /usr/local/bin/comp-relay; rm -f /tmp/comp-relay; fi; \
      sudo mkdir -p /srv/comp /etc/comp /etc/caddy/comp; \
      sudo chmod 0711 /etc/comp; \
      /usr/local/bin/comp-host --help >/dev/null && echo '  comp-host installed:' && /usr/local/bin/comp-host --help | head -1"
    @# Caddy only reads files it is told to. An import line at the end of the Caddyfile
    @# is top-level, which is where site blocks belong.
    ssh {{host}} "set -e; \
      sudo touch /etc/caddy/Caddyfile; \
      grep -qF 'import /etc/caddy/comp/*.caddy' /etc/caddy/Caddyfile || \
        echo 'import /etc/caddy/comp/*.caddy' | sudo tee -a /etc/caddy/Caddyfile >/dev/null; \
      sudo caddy validate --config /etc/caddy/Caddyfile 2>&1 | tail -2 || true"
    just selfhost-tsip {{host}}
    @echo "  bootstrapped {{host}} — `just selfhost-deploy <app> {{host}}` will work now"

# Render one app's systemd unit, env file and route WITHOUT touching a box.
# Read the output before you trust it to a server.
selfhost-render app router="caddy": build-selfhost
    ./cli/target/release/comp node render apps/{{app}}.toml \
      --out target/selfhost --router {{router}}
    @echo "--- unit ---";  cat target/selfhost/{{app}}/comp-{{app}}.service
    @echo "--- route ---"; cat target/selfhost/{{app}}/{{app}}.*

# Derive a spec for every app that has a `host-<app>` recipe but no `apps/*.toml`.
#
# The recipe already names the artifact, the port and the SPA directory — a spec is
# those three facts plus a hostname. What the generator cannot know it does not
# guess: `domain` is a placeholder to edit, and `access` is left at its
# fail-closed default. An existing spec is never overwritten.
selfhost-specs:
    python3 tools/gen-app-specs.py
    @just selfhost-check

# Refuse the collisions a single spec cannot see: two apps on one port, one
# domain, or one name. Run it in CI over apps/*.toml.
selfhost-check: build-selfhost
    ./cli/target/release/comp node validate apps/*.toml

# Ship one app to one box: compose, render, copy, restart, route.
#
# Needs on the box: comp-host at /usr/local/bin, caddy (or traefik), and an ssh
# key. It is deliberately plain scp+systemctl — for a handful of machines a
# control plane costs more than it saves (docs/SELFHOST.md).
selfhost-deploy app host router="caddy": build-selfhost
    #!/usr/bin/env bash
    set -euo pipefail
    ART=$(python3 -c "import tomllib,sys;print(tomllib.load(open('apps/{{app}}.toml','rb'))['artifact'])")
    if [ ! -f "$ART" ]; then
      echo "missing $ART — run the app's compose recipe first (just compose-{{app}})" >&2
      exit 1
    fi
    ./cli/target/release/comp node validate apps/*.toml
    # Fail here, not with a unit that cannot start. The binary is the one thing the
    # deploy does NOT ship — it is 38 MB and identical for every app on the box.
    if ! ssh {{host}} "test -x /usr/local/bin/comp-host"; then
      echo "{{host}} has no /usr/local/bin/comp-host — run: just selfhost-bootstrap {{host}}" >&2
      exit 1
    fi
    ./cli/target/release/comp node render apps/{{app}}.toml --out target/selfhost --router {{router}}
    D=target/selfhost/{{app}}
    # 0644 on the artifact: the unit runs under DynamicUser, a transient uid that
    # must still be able to read it. 0600 on the env file: it may hold secrets and
    # systemd reads it as root before dropping privileges.
    ssh {{host}} "sudo mkdir -p /srv/comp/{{app}} /etc/comp /etc/caddy/comp"
    scp "$ART" {{host}}:/tmp/{{app}}.wasm
    scp "$D/comp-{{app}}.service" {{host}}:/tmp/
    scp "$D/{{app}}.env" {{host}}:/tmp/
    # Rendered only when the spec declares [triggers].
    if [ -f "$D/comp-{{app}}-relay.service" ]; then scp "$D/comp-{{app}}-relay.service" {{host}}:/tmp/; fi
    scp "$D"/{{app}}.caddy {{host}}:/tmp/ 2>/dev/null || scp "$D"/{{app}}.yml {{host}}:/tmp/
    ssh {{host}} "set -e; \
      sudo install -m 0644 /tmp/{{app}}.wasm /srv/comp/{{app}}/app.wasm; \
      sudo install -m 0600 /tmp/{{app}}.env /etc/comp/{{app}}.env; \
      sudo install -m 0644 /tmp/comp-{{app}}.service /etc/systemd/system/comp-{{app}}.service; \
      if [ -f /tmp/comp-{{app}}-relay.service ]; then \
        sudo install -m 0644 /tmp/comp-{{app}}-relay.service /etc/systemd/system/comp-{{app}}-relay.service; \
      fi; \
      if [ -f /tmp/{{app}}.serve.sh ]; then \
        sudo install -m 0755 /tmp/{{app}}.serve.sh /srv/comp/{{app}}/serve.sh; \
      elif [ -f /tmp/{{app}}.caddy ]; then \
        sudo install -m 0644 /tmp/{{app}}.caddy /etc/caddy/comp/{{app}}.caddy; \
      else \
        sudo install -m 0644 /tmp/{{app}}.yml /etc/traefik/comp/{{app}}.yml; \
      fi; \
      sudo systemctl daemon-reload; \
      sudo systemctl enable --now comp-{{app}}; \
      sudo systemctl restart comp-{{app}}; \
      if [ -f /etc/systemd/system/comp-{{app}}-relay.service ]; then \
        sudo systemctl enable --now comp-{{app}}-relay; sudo systemctl restart comp-{{app}}-relay; \
      fi; \
      if [ -f /srv/comp/{{app}}/serve.sh ]; then sudo /srv/comp/{{app}}/serve.sh; fi; \
      rm -f /tmp/{{app}}.wasm /tmp/comp-{{app}}.service /tmp/comp-{{app}}-relay.service \
            /tmp/{{app}}.env /tmp/{{app}}.caddy /tmp/{{app}}.yml /tmp/{{app}}.serve.sh"
    # Only a tailnet app needs TS_IP. Pinning it for a public app would report a
    # missing tailscale on a box that has no reason to have one, which reads as a
    # failed deploy when nothing is wrong.
    ACCESS=$(python3 -c "import tomllib;print(tomllib.load(open('apps/{{app}}.toml','rb')).get('access','tailnet'))")
    if [ "$ACCESS" = "tailnet" ]; then just selfhost-tsip {{host}}; fi
    ssh {{host}} "sudo systemctl reload caddy 2>/dev/null || sudo systemctl restart caddy 2>/dev/null || true"
    @echo "deployed {{app}} to {{host}} [$ACCESS]"

# Pin Caddy's TS_IP to this box's Tailscale address, so that `bind {$TS_IP}` in a
# tailnet route means what it says.
#
# This matters more than it looks: Caddy substitutes {$TS_IP} from its OWN
# environment, so if nothing sets it the bind resolves to empty and Caddy listens on
# EVERY interface — the exact opposite of private, silently. Idempotent; on a box with
# no tailscale it reports that and changes nothing.
selfhost-tsip host:
    #!/usr/bin/env bash
    set -uo pipefail
    IP=$(ssh {{host}} "tailscale ip -4 2>/dev/null | head -1" || true)
    if [ -z "$IP" ]; then
      echo "  {{host}}: no tailscale address — a tailnet route here would bind nothing." >&2
      echo "  Install tailscale, or set access = \"public\" for apps on this box." >&2
      exit 0
    fi
    ssh {{host}} "set -e; \
      sudo mkdir -p /etc/systemd/system/caddy.service.d; \
      printf '[Service]\nEnvironment=TS_IP=%s\n' '$IP' | \
        sudo tee /etc/systemd/system/caddy.service.d/ts-ip.conf >/dev/null; \
      sudo systemctl daemon-reload"
    echo "  {{host}}: Caddy TS_IP=$IP (tailnet routes bind there only)"

# Is it up, and what is it doing?
selfhost-status app host:
    ssh {{host}} "systemctl status comp-{{app}} --no-pager -n 15 || true"

# Remove an app from a box, including its state.
selfhost-remove app host:
    ssh {{host}} "set -e; \
      sudo systemctl disable --now comp-{{app}}-relay || true; \
      sudo systemctl disable --now comp-{{app}} || true; \
      sudo rm -f /etc/systemd/system/comp-{{app}}-relay.service \
                 /etc/systemd/system/comp-{{app}}.service /etc/comp/{{app}}.env \
                 /etc/caddy/comp/{{app}}.caddy /etc/traefik/comp/{{app}}.yml; \
      sudo rm -rf /srv/comp/{{app}} /var/lib/private/comp/{{app}} /var/lib/comp/{{app}}; \
      sudo systemctl daemon-reload; sudo systemctl reload caddy 2>/dev/null || true"
    @echo "removed {{app}} from {{host}} (including its state)"

# Ship every app in apps/ to one box. The fleet-of-one-machine case.
selfhost-deploy-all host router="caddy":
    #!/usr/bin/env bash
    set -euo pipefail
    for f in apps/*.toml; do
      just selfhost-deploy "$(basename "$f" .toml)" {{host}} {{router}}
    done

# ---- the wasmCloud lanes: same app, someone else's runtime -------------------
#
# For a cluster somebody else already operates. ADR-0021 took Kubernetes off this
# platform's runtime path deliberately and priced it, so this is interop, not a
# recommendation — `docs/SELFHOST.md` still says start at tier 1.
#
# Two axes, four manifests, one app spec:
#   --topology fused|linked    one wac-composed artifact, or components wired by links
#   --api      v1|v2           core.oam.dev/v1beta1 (wadm 0.21), or wasmcloud.dev/v1alpha1
#
# No `wash`: wash 2.x removed `wash app put`, but the API underneath it is a set of
# NATS subjects both versions still serve, and those are what these recipes use.

wasmcloud_lattice := env_var_or_default("WASMCLOUD_LATTICE", "default")
wasmcloud_registry := env_var_or_default("WASMCLOUD_REGISTRY", "registry.wasmcloud.svc.cluster.local:5000")
# Where THIS machine reaches the registry to push. In-cluster the host pulls from
# the name above; from a laptop that name does not resolve, so pushes go via the
# NodePort. Two names for one registry, and conflating them is why a push succeeds
# and a pull then fails.
wasmcloud_push_registry := env_var_or_default("WASMCLOUD_PUSH_REGISTRY", "localhost:30500")

# The capability graph, derived from the BUILT artifacts. A linked render needs it:
# a wadm link carries the WIT namespace, package and interfaces that say which import
# it satisfies, and wadm refuses one that names only a target.
capgraph-json:
    @mkdir -p target
    @cd reconciler && cargo build --release --quiet --bin comp-capgraph
    ./reconciler/target/release/comp-capgraph --format json > target/capgraph.json

# Render one app as a wadm manifest. Touches no cluster.
#
#   just wasmcloud-render gate                     # fused, v1
#   just wasmcloud-render gate linked v2
wasmcloud-render app topology="fused" api="v1" addr="0.0.0.0:8080": build-selfhost capgraph-json
    ./cli/target/release/holon wadm render apps/{{app}}.toml \
      --topology {{topology}} --api {{api}} --graph target/capgraph.json \
      --registry {{wasmcloud_registry}} --addr {{addr}} \
      --out target/wadm/{{app}}.yaml
    @cat target/wadm/{{app}}.yaml

# Push an app's artifact to the cluster registry, by digest.
#
# `wkg` rather than a bundled pusher: reconciler/src/oci.rs exists and is proven, but
# it is on the reconciler's distribution path, and this lane wants one artifact now.
# The media types match either way — that is what oci.rs was written to guarantee.
wasmcloud-push app: build-selfhost
    #!/usr/bin/env bash
    set -euo pipefail
    ART=$(python3 -c "import tomllib;print(tomllib.load(open('apps/{{app}}.toml','rb'))['artifact'])")
    if [ ! -f "$ART" ]; then
      echo "missing $ART — run: just compose-{{app}}" >&2
      exit 1
    fi
    wkg oci push {{wasmcloud_push_registry}}/{{app}}:latest "$ART" --insecure {{wasmcloud_push_registry}}

# Render, push and deploy one app to a wasmCloud lattice.
#
#   WASMCLOUD_LATTICE=vet-clinic just wasmcloud-deploy graphviz
#
# The lattice must have a host in it: wadm answers "0/1 eligible hosts found" when
# the manifest lands somewhere no host is listening, which looks like a manifest
# error and is not one.
wasmcloud-deploy app topology="fused" api="v1" addr="0.0.0.0:8080": (wasmcloud-render app topology api addr) (wasmcloud-push app)
    #!/usr/bin/env bash
    set -euo pipefail
    L={{wasmcloud_lattice}}
    python3 -c "import json,yaml;print(json.dumps(yaml.safe_load(open('target/wadm/{{app}}.yaml'))))" \
      > target/wadm/{{app}}.json
    tools/wadm.sh "wadm.api.$L.model.put" target/wadm/{{app}}.json | head -2
    tools/wadm.sh "wadm.api.$L.model.deploy.{{app}}" | head -2
    echo "deployed {{app}} to lattice $L — just wasmcloud-status {{app}}"

# What did the cluster make of it? One line per scaler.
wasmcloud-status app:
    @tools/wadm.sh "wadm.api.{{wasmcloud_lattice}}.model.status.{{app}}" | python3 tools/wadm-status.py

wasmcloud-remove app:
    #!/usr/bin/env bash
    set -uo pipefail
    L={{wasmcloud_lattice}}
    tools/wadm.sh "wadm.api.$L.model.undeploy.{{app}}" | head -1
    printf '{}' > target/wadm/.del.json
    tools/wadm.sh "wadm.api.$L.model.del.{{app}}" target/wadm/.del.json | head -1

# ---- wasmCloud 2.x: a Workload, not a manifest -------------------------------
#
# 2.x dropped wadm and OAM entirely, so this lane does not go through wadm at all:
# `holon wadm render --api v2` emits a `runtime.wasmcloud.dev/v1alpha1` Workload and
# kubectl applies it. The operator schedules it onto a host in a host group.
#
# Install the stack once:
#   helm install wasmcloud-v2 oci://ghcr.io/wasmcloud/charts/runtime-operator \
#     --version 2.8.0 --namespace wasmcloud-v2 --create-namespace
#
# A release 2.x host provides standard WASI and wasmcloud:messaging and nothing
# else — no keyvalue backend, no wasi:config store, and no `comp:` interface, which
# needs a host component plugin that release images are not built with. An app that
# imports one is REFUSED at render time with the reason.

wasmcloud_v2_namespace := env_var_or_default("WASMCLOUD_V2_NAMESPACE", "wasmcloud-v2")

# Render an app as a wasmCloud 2.x Workload. Touches no cluster.
wasmcloud-v2-render app: build-selfhost capgraph-json
    ./cli/target/release/holon wadm render apps/{{app}}.toml --api v2 \
      --namespace {{wasmcloud_v2_namespace}} --graph target/capgraph.json \
      --registry {{wasmcloud_registry}} --out target/wadm/{{app}}.v2.yaml
    @cat target/wadm/{{app}}.v2.yaml

# Push and apply one app to the 2.x stack.
wasmcloud-v2-deploy app: (wasmcloud-v2-render app) (wasmcloud-push app)
    kubectl apply -f target/wadm/{{app}}.v2.yaml
    @echo "applied — just wasmcloud-v2-status {{app}}"

# READY says whether the host actually linked and started it. False with no error
# above usually means an import the host cannot satisfy; the host log has the reason.
wasmcloud-v2-status app:
    @kubectl get workload {{app}} -n {{wasmcloud_v2_namespace}} 2>&1 || true
    @kubectl logs -n {{wasmcloud_v2_namespace}} deploy/hostgroup-default --since=2m 2>/dev/null \
      | grep -i "{{app}}" | grep -iE "error|warn" | tail -3 || true

wasmcloud-v2-remove app:
    kubectl delete workload {{app}} -n {{wasmcloud_v2_namespace}} --ignore-not-found

# Render the operator's host, for the Kubernetes lane. The Application manifest is
# the SAME one wash would deploy — only the driver differs, which is what makes this
# lane one extra file rather than a second renderer.
wasmcloud-host namespace="holon" lattice="holon" version="1.6.0": build-selfhost
    ./cli/target/release/holon wadm host --namespace {{namespace}} --lattice {{lattice}} \
      --version {{version}} --registry {{wasmcloud_registry}}

# Talk to wadm over NATS from inside the cluster, since `wash` may not be installed
# and wash 2.x removed the command the old manifests still document.
#
# A shell script rather than a `just` recipe, because a manifest is JSON full of
# quotes and braces: interpolating it into a command line hands it to the shell to
# re-parse (it fails on the first `(` in a description), and `just` does not forward
# stdin to a recipe either. So the payload goes through a file, which has neither
# problem.
#
#   tools/wadm.sh <subject> [payload-file]

# ---- the lattice lane (tier 2/3): many boxes, one control loop ---------------
#
# Tier 1 above puts ONE app in one comp-host behind a proxy, with no control plane
# at all. This is the tier where placement stops being your decision: a comp-host
# per node holding every app, a reconciler converging them, an ingress routing by
# Host header. Reach for it when choosing which box an app runs on has become a
# chore — with two or three machines, tier 1 is the cheaper answer.
#
# `just host-platform` is this same topology on localhost. These recipes are that
# recipe with `trap kill` replaced by `Restart=always`.

# Cross-build the two control binaries, static, for a Linux box. `comp-host` comes
# from `selfhost-build-host` — it is the same binary in both tiers, and building it
# twice would be two answers to one question.
lattice-build arch="x86_64":
    cd reconciler && cross build --release --target {{arch}}-unknown-linux-musl \
      --bin comp-reconciler --bin comp-ingress
    @ls -la reconciler/target/{{arch}}-unknown-linux-musl/release/comp-reconciler | awk '{printf "  comp-reconciler %.0f MB\n", $5/1048576}'
    @ls -la reconciler/target/{{arch}}-unknown-linux-musl/release/comp-ingress | awk '{printf "  comp-ingress    %.0f MB\n", $5/1048576}'

# Read the units before trusting them to a server. Touches no box.
lattice-render spec="fleet.toml" out="target/fleet": build-selfhost
    ./cli/target/release/holon fleet render {{spec}} --out {{out}}
    @for d in {{out}}/*/; do echo "--- $d"; for f in "$d"*.service; do echo "  $(basename $f)"; done; done

# Check a fleet spec: node names, reachable addresses, and a lease that outlives a
# pass. Run it in CI beside `selfhost-check`.
lattice-check spec="fleet.toml": build-selfhost
    ./cli/target/release/holon fleet validate {{spec}}

# ONE-TIME per box. Installs the three binaries every lattice role might need.
#
# All three go on every box on purpose: which role a box plays is the fleet spec's
# decision, and a box that gains a reconciler later should not need a second visit.
lattice-bootstrap host arch="x86_64": (selfhost-build-host arch) (lattice-build arch)
    #!/usr/bin/env bash
    set -euo pipefail
    R=reconciler/target/{{arch}}-unknown-linux-musl/release
    scp host/target/{{arch}}-unknown-linux-musl/release/comp-host "$R/comp-reconciler" "$R/comp-ingress" {{host}}:/tmp/
    ssh {{host}} "set -e; \
      sudo install -m 0755 /tmp/comp-host /tmp/comp-reconciler /tmp/comp-ingress /usr/local/bin/; \
      rm -f /tmp/comp-host /tmp/comp-reconciler /tmp/comp-ingress; \
      sudo mkdir -p /etc/comp; sudo chmod 0711 /etc/comp; \
      /usr/local/bin/comp-reconciler --help >/dev/null && echo '  three binaries installed'"
    @echo "  bootstrapped {{host}} — it can now play any lattice role"

# Ship the rendered units to the boxes the fleet spec names.
#
# The BOX NAME in the spec is what gets ssh'd to, so `host = "edge"` means an ssh
# host called `edge`. That is deliberate: it is the same string you already type.
lattice-deploy spec="fleet.toml" out="target/fleet": build-selfhost
    #!/usr/bin/env bash
    set -euo pipefail
    ./cli/target/release/holon fleet validate {{spec}}
    ./cli/target/release/holon fleet render {{spec}} --out {{out}} >/dev/null
    for d in {{out}}/*/; do
      BOX=$(basename "$d")
      if ! ssh "$BOX" "test -x /usr/local/bin/comp-reconciler"; then
        echo "$BOX is not bootstrapped — run: just lattice-bootstrap $BOX" >&2
        exit 1
      fi
      for f in "$d"*.service; do
        scp "$f" "$BOX:/tmp/$(basename "$f")"
      done
      # 0600 and root-owned: systemd reads it before dropping privileges, and it
      # holds the platform secret. Never overwritten — a re-deploy must not blank a
      # secret somebody filled in on the box.
      if [ -f "$d/reconciler.env" ]; then
        scp "$d/reconciler.env" "$BOX:/tmp/reconciler.env"
        ssh "$BOX" "test -f /etc/comp/reconciler.env || sudo install -m 0600 /tmp/reconciler.env /etc/comp/reconciler.env; rm -f /tmp/reconciler.env"
      fi
      ssh "$BOX" "set -e; \
        for u in /tmp/*.service; do sudo install -m 0644 \"\$u\" /etc/systemd/system/; done; \
        sudo systemctl daemon-reload; \
        for u in /tmp/*.service; do n=\$(basename \"\$u\"); sudo systemctl enable --now \"\$n\"; sudo systemctl restart \"\$n\"; done; \
        rm -f /tmp/*.service"
      echo "  deployed $(ls "$d"*.service | xargs -n1 basename | tr '\n' ' ') to $BOX"
    done
    @echo "fleet deployed. A reconciler with an empty PLATFORM_SECRET will not converge —"
    @echo "fill /etc/comp/reconciler.env on each control box, then: systemctl restart comp-reconciler"

# Is it up, and which reconciler holds the lease?
lattice-status spec="fleet.toml" out="target/fleet": build-selfhost
    #!/usr/bin/env bash
    set -uo pipefail
    ./cli/target/release/holon fleet render {{spec}} --out {{out}} >/dev/null 2>&1
    for d in {{out}}/*/; do
      BOX=$(basename "$d")
      echo "== $BOX"
      for f in "$d"*.service; do
        ssh "$BOX" "systemctl is-active $(basename "$f") 2>/dev/null | sed 's/^/   $(basename "$f"): /'" || true
      done
    done

# Remove every lattice unit from every box the spec names. Leaves the binaries.
lattice-remove spec="fleet.toml" out="target/fleet": build-selfhost
    #!/usr/bin/env bash
    set -uo pipefail
    ./cli/target/release/holon fleet render {{spec}} --out {{out}} >/dev/null 2>&1
    for d in {{out}}/*/; do
      BOX=$(basename "$d")
      for f in "$d"*.service; do
        n=$(basename "$f")
        ssh "$BOX" "sudo systemctl disable --now $n 2>/dev/null; sudo rm -f /etc/systemd/system/$n" || true
      done
      ssh "$BOX" "sudo systemctl daemon-reload" || true
      echo "  removed lattice units from $BOX"
    done
    @echo "-- left behind ON PURPOSE: /etc/comp/reconciler.env (a secret you filled in)"
    @echo "   and the binaries in /usr/local/bin."

# ADR-0023's falsifying measurement, ADR-0026's numbers: two tenants in ONE
# comp-host process, one of them hostile, isolation and throughput from the SAME
# run. Needs nats-server and oha; needs no cluster and no second machine.
#
#   just adversarial                                    # address backstop (the real test)
#
# The second variant, with egress denied outright rather than allow-listed — so the
# adversary cannot reach the bus it would otherwise use to talk around the host
# (ADR-0008). `run.sh` reads SPECS; it has never read a MANIFEST, and the JSON this
# line used to name has never existed in this repository:
#
#   SPECS="--spec fixtures/two-tenants-denyall-eve.yaml \
#          --spec fixtures/two-tenants-denyall-alice.yaml" just adversarial
#
# Re-run this when the linker gains a capability, a kv backend changes, or anyone
# proposes relaxing the address deny-list.
adversarial: compose-gate build-reconciler
    cd components && cargo component build -p adversary --release --target wasm32-wasip2
    cd host && cargo build --release --bin comp-host
    bash bench/adversarial/run.sh

# Stage what `bench:inproc` needs, so it runs from a CLEAN CHECKOUT.
#
# It did not. The in-process benchmark imports transpiled `gen/` from 34 jco
# examples, and seven of their `.wasm` inputs are absent from a fresh clone: they
# are build outputs (`.gitignore` has `**/*.wasm`), while the other 27 are tracked
# from before that rule existed. The committed `results-inproc.json` includes the
# ops those seven provide, so the numbers were real — measured on a machine that
# happened to have them lying around, which is the `goal-demo.sh` failure again
# (ADR-0057: a number whose label describes something other than what ran).
#
# Committing 1.2 MB of binaries would have "fixed" it against the repo's own
# intent. Staging them from what `just build` already produces is the same result
# with nothing new tracked.
#
#   just bench-setup && (cd bench && npm run bench:inproc)
bench-setup: build compose-webhook
    #!/usr/bin/env bash
    set -euo pipefail
    R=components/target/wasm32-wasip2/release
    # Two of these want the COMPOSED artifact, not the bare component, because the
    # bare one leaves non-WASI imports for jco to emit as bare specifiers — which
    # Node then rejects outright as a URL scheme (`protocol 'audit:'`). `_derive`
    # says which: auth-guard imports ratelimit:guard + audit:log/recorder, and both
    # it and audit-log import audit:log/types, a TYPES-ONLY interface nothing
    # exports and composition therefore cannot satisfy. That last one is mapped to
    # a stub at transpile time; see the shims the package.json files point at.
    just _derive audit-log components/target/audit_log.composed.wasm
    just _derive auth-guard components/target/auth_guard.composed.wasm
    cp "$R/blob_store.wasm"         examples/jco-blob/blob_store.wasm
    cp "$R/cache.wasm"              examples/jco-cache/cache.wasm
    cp "$R/feature_flags.wasm"      examples/jco-featureflags/feature_flags.wasm
    cp "$R/idempotency_guard.wasm"  examples/jco-idempotency/idempotency_guard.wasm
    cp components/target/audit_log.composed.wasm  examples/jco-audit/audit_log.wasm
    cp components/target/auth_guard.composed.wasm examples/jco-embed/auth_guard.wasm
    cp components/target/webhook_ingest.composed.wasm examples/jco-webhook/webhook_ingest.wasm
    # Every example the bench imports, installed and transpiled. Derived from the
    # bench source rather than listed here: a hand-kept list of 34 goes stale on the
    # next import and fails as a missing module three files away from the cause.
    EX=$(node -e 'const fs=require("fs");const s=fs.readFileSync("bench/src/bench-inproc.ts","utf8");console.log([...new Set([...s.matchAll(/examples\/(jco-[a-z0-9-]+)\//g)].map(m=>m[1]))].sort().join(" "))')
    n=0
    for e in $EX; do
      ( cd "examples/$e" && npm install --silent --no-audit --no-fund && npm run transpile --silent >/dev/null )
      n=$((n+1))
    done
    echo "staged + transpiled $n example(s) — now: cd bench && npm install && npm run bench:inproc"

# `shared-state`, `five-nodes` and `split-graph` were recipes here. Their scripts
# were deleted when the scenarios became Rust tests, and the recipes were not — so
# for three commits `just shared-state` failed with "No such file or directory"
# rather than telling anyone where the coverage went. Where it went:
#
#   shared-state  -> reconciler/tests/state.rs   (both halves: nats continues,
#                    node-local is refused; `cargo nextest run -E 'test(state)'`)
#   five-nodes    -> reconciler/tests/ha.rs for the replica/ingress half, and
#                    bench/failover/cross-machine.sh for the two-machine half
#   split-graph   -> nothing. Cross-node invocation (ADR-0032) has no test and no
#                    script; `fixtures/split-graph.yaml` is the input one would
#                    take. Named here rather than quietly dropped.

# Round robin's weak case, on a real heterogeneous fleet: 2 Mac nodes + 2 Pi nodes,
# same load, both algorithms. See docs/adr/0030 — the difference was 10x.
slow-backend: compose-gate build-reconciler
    cd host && cargo build --release --bin comp-host
    bash bench/adversarial/slow-backend.sh

# Organisations end to end: three people, one shared org, and a fourth party
# refused at both read and write. See docs/adr/0031.
orgs: compose-platform build-reconciler
    cd host && cargo build --release --bin comp-host
    bash bench/adversarial/orgs.sh

# Multi-tenant benchmark: 2 organisations, 5 members each, both deploying and both
# under load at once — control plane cost, data plane throughput, and whether
# isolation survives the load, all from ONE run. See docs/adr/0033.
#
#   just tenancy-bench                       # 3 nodes, 20s, 40 conns per org
#   NODES=5 DURATION=60s CONNS=100 just tenancy-bench
tenancy-bench: compose-platform compose-gate build-reconciler
    cd host && cargo build --release --bin comp-host
    cd cli && cargo build --release
    bash bench/tenancy/run.sh

# ---- durability -----------------------------------------------------------
#
# Everything a tenant owns lives in JetStream KV, and until now nothing copied it
# anywhere. `history: 1` means there is not even a previous version to go back to,
# so a bad migration, a bad write or an `rm -rf` on the JetStream directory was
# final. These two recipes are the floor: not a backup STRATEGY, but the thing
# that makes one possible.
#
#   just backup                                  # every KV bucket -> backups/<utc>/
#   DIR=backups/2026-08-11T18-00-00Z just restore
#   DIR=... REPLICAS=3 just restore              # and re-replicate on the way in
#
# `nats stream backup` is the vendor's own snapshot protocol — it streams the
# stream's messages and its configuration, and `restore` recreates both. Writing
# our own would be re-implementing a wire format to no benefit.
backup:
    #!/usr/bin/env bash
    set -euo pipefail
    URL="${NATS_URL:-nats://127.0.0.1:4222}"
    DIR="${DIR:-backups/$(date -u +%Y-%m-%dT%H-%M-%SZ)}"
    mkdir -p "$DIR"
    # KV buckets are streams named `KV_<bucket>`. Backing up by that prefix takes
    # every tenant's store and nothing else — the inventory bucket included, which
    # is derived state but costs nothing to carry.
    streams=$(nats --server "$URL" stream ls -n 2>/dev/null | grep '^KV_' || true)
    if [ -z "$streams" ]; then
      echo "no KV buckets on $URL — nothing to back up"; exit 0
    fi
    n=0
    for s in $streams; do
      nats --server "$URL" stream backup "$s" "$DIR/$s" >/dev/null
      n=$((n+1))
      echo "  $s"
    done
    # A manifest, so a restore does not depend on guessing what was in here.
    printf '{"taken":"%s","url":"%s","streams":%s}\n' \
      "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$URL" \
      "$(printf '%s\n' $streams | awk 'BEGIN{printf "["} {printf "%s\"%s\"", (NR>1?",":""), $0} END{print "]"}')" \
      > "$DIR/manifest.json"
    echo "backed up $n bucket(s) to $DIR"

# Restore what `just backup` wrote. Refuses to clobber a bucket that already
# exists — restoring over live data is how a backup turns into an outage, and the
# operator who wants that can delete the stream first and say so.
restore:
    #!/usr/bin/env bash
    set -euo pipefail
    URL="${NATS_URL:-nats://127.0.0.1:4222}"
    DIR="${DIR:?set DIR=backups/<stamp>}"
    [ -f "$DIR/manifest.json" ] || { echo "no manifest.json in $DIR"; exit 1; }
    existing=$(nats --server "$URL" stream ls -n 2>/dev/null | grep '^KV_' || true)
    n=0
    for path in "$DIR"/KV_*; do
      [ -d "$path" ] || continue
      s=$(basename "$path")
      if printf '%s\n' $existing | grep -qx "$s"; then
        echo "  SKIP $s — it already exists. Delete it first if you mean to replace it."
        continue
      fi
      # The stream name comes from the backup itself; `restore` takes only the
      # directory. `REPLICAS=` overrides how many copies to recreate, which makes
      # a restore the way to change replication on an existing bucket too.
      nats --server "$URL" stream restore "$path" ${REPLICAS:+--replicas "$REPLICAS"} >/dev/null
      n=$((n+1))
      echo "  $s"
    done
    echo "restored $n bucket(s) from $DIR to $URL"

# Put one of the repository's authored goals in place for `goal-run`.
#
# `goalrun` reads `.comp/goal.toml` and only that path (ADR-0082 wants the goal
# versioned in the repo it acts on). Several goals therefore live side by side in
# `.comp/goals/` and one is copied into place — visible in the run's own diff, which
# is the point: which goal a run answered is not a shell variable that scrolled away.
#
#   just goal-use triage-assist && CHECKOUT=. REPO=owner/name … just goal-run
goal-use app="":
    #!/usr/bin/env bash
    set -euo pipefail
    src=".comp/goals/{{app}}.toml"
    if [ -z "{{app}}" ] || [ ! -f "$src" ]; then
      [ -n "{{app}}" ] && echo "no goal at $src. Authored goals:" || echo "which goal? Authored:"
      for f in .comp/goals/*.toml; do
        [ -e "$f" ] || continue
        printf '  %s\n' "$(basename "$f" .toml)"
      done
      exit 1
    fi
    cp "$src" .comp/goal.toml
    echo "goal: $(python3 -c "import re,sys;t=open('.comp/goal.toml').read();m=re.search(r'^title = \"(.*)\"',t,re.M);print(m.group(1) if m else '{{app}}')")"
    echo "  .comp/goals/{{app}}.toml -> .comp/goal.toml"

# ---- graph-engineering: run a goal to a pull request -----------------------
#
# The one command behind the showcase. Builds the components and the native
# binaries, then drives a real search (real model, real gate, real forge) over a
# checked-out repo and opens a PR for the winner.
#
#   just goal-run \
#     checkout=/path/to/repo repo=owner/name \
#     anthropic_key=~/.secrets/anthropic github_token=~/.secrets/ghpat
#
# Inputs come from the ENVIRONMENT, not just variables — immune to just's
# override rules, argument ordering, and any stale justfile a nearby repo might
# shadow this with. Secrets are FILE PATHS, never values: nothing sensitive
# reaches argv.
#
#   CHECKOUT=/path/to/repo REPO=owner/name \
#   ANTHROPIC_KEY=~/.comp-secrets/anthropic GITHUB_TOKEN=~/.comp-secrets/ghpat \
#   SMOKE=1 just goal-run
#
# Optional: BRANCHES, ROUNDS, MODEL, ATTEMPTS, DRY_RUN=1, SMOKE=1.
#
# BASE_URL points inference somewhere other than the real API — the shim being
# the reason it exists. A private address also needs
# COMP_FLEET_ALLOW_PRIVATE_EGRESS=1, which is inherited from the environment and
# deliberately not set here (see `claude-shim`).
#
#   just claude-shim &
#   CHECKOUT=… REPO=… ANTHROPIC_KEY=… GITHUB_TOKEN=… \
#   BASE_URL=http://127.0.0.1:8787 COMP_FLEET_ALLOW_PRIVATE_EGRESS=1 just goal-run
# Drain a project's queue instead of typing `goal run` per goal.
#
# A person still starts each goal (console, or `holon goal start`); this picks up
# what is in `running` and drives it to a PR, MAX_RUNS at a time. Everything after
# `--` goes to comp-goalrun, so the model/pool/budget flags are the same ones.
#
#   set -a; source .comp/csatapaci.env; set +a
#   just openai-shim &
#   PROJECT=holon CHECKOUT=$PWD REPO=me/holon just goald
goald:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PROJECT:?set PROJECT=<project name>}"
    : "${CHECKOUT:?set CHECKOUT=/path/to/repo}"
    : "${REPO:?set REPO=owner/name}"
    : "${ANTHROPIC_KEY:?set ANTHROPIC_KEY=/path/to/keyfile}"
    : "${GITHUB_TOKEN:?set GITHUB_TOKEN=/path/to/tokenfile}"
    cd reconciler && cargo build --release --bins && cd ..
    ck="${CHECKOUT/#\~/$HOME}"; ak="${ANTHROPIC_KEY/#\~/$HOME}"; gt="${GITHUB_TOKEN/#\~/$HOME}"
    export COMP_GOALRUN_BIN="$PWD/reconciler/target/release/comp-goalrun"
    export COMP_FLEET_ALLOW_PRIVATE_EGRESS=1
    run=(--anthropic-key "$ak" --github-token "$gt" \
         --model "${OPENAI_MODEL:?source .comp/csatapaci.env}" \
         --answer-model "$OPENAI_MODEL" \
         --anthropic-base-url "${HOLON_BASE_URL:-http://127.0.0.1:8787}" \
         --branches "${BRANCHES:-4}" --rounds "${ROUNDS:-1}" \
         --timeout "${GOAL_TIMEOUT:-900}")
    # The shared pool is what makes the goals aware of each other's work.
    [ -n "${SURREAL_URL:-}" ] && run+=(--surreal-url "$SURREAL_URL")
    exec reconciler/target/release/comp-goald \
      --project "$PROJECT" --checkout "$ck" --repo "$REPO" \
      --max-runs "${MAX_RUNS:-1}" --poll "${POLL:-15}" -- "${run[@]}"

goal-run:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${CHECKOUT:?set CHECKOUT=/path/to/repo}"
    : "${REPO:?set REPO=owner/name}"
    : "${ANTHROPIC_KEY:?set ANTHROPIC_KEY=/path/to/keyfile}"
    : "${GITHUB_TOKEN:?set GITHUB_TOKEN=/path/to/tokenfile}"
    just build
    cd host && cargo build --release --bin comp-host && cd ..
    cd reconciler && cargo build --release --bins && cd ..
    # Expand a leading ~ that a quoted env value keeps literal.
    ck="${CHECKOUT/#\~/$HOME}"; ak="${ANTHROPIC_KEY/#\~/$HOME}"; gt="${GITHUB_TOKEN/#\~/$HOME}"
    args=(--checkout "$ck" --repo "$REPO" --anthropic-key "$ak" --github-token "$gt" \
          --branches "${BRANCHES:-4}" --rounds "${ROUNDS:-1}" \
          --model "${MODEL:-claude-haiku-4-5-20251001}" --attempts "${ATTEMPTS:-2}")
    [ -n "${BASE_URL:-}" ] && args+=(--anthropic-base-url "$BASE_URL")
    # A multi-part goal needs the contract registry; a one-part goal never does.
    [ -n "${SURREAL_URL:-}" ] && args+=(--surreal-url "$SURREAL_URL")
    [ -n "${SURREAL_PASSWORD:-}" ] && args+=(--surreal-password "${SURREAL_PASSWORD/#\~/$HOME}")
    # Through the shim a call is a whole `claude -p`, so the default 300s is short.
    [ -n "${TIMEOUT:-}" ] && args+=(--timeout "$TIMEOUT")
    # A measured run pins this to 1.0 — never skip. At the default 0.9 a re-run after
    # a HARNESS failure is skipped as work already done, which reads as a pass.
    [ -n "${SKIP_ABOVE:-}" ] && args+=(--skip-above "$SKIP_ABOVE")
    [ "${DRY_RUN:-0}" = "1" ] && args+=(--dry-run)
    [ "${SMOKE:-0}" = "1" ] && args+=(--smoke)
    ./reconciler/target/release/comp-goalrun "${args[@]}"

graphviz_composed := "components/target/graphviz_domain.composed.wasm"

compose-graphviz: build
    @just _derive graph-viz-domain {{graphviz_composed}}

host-graphviz: compose-graphviz
    cd host && cargo run --release --bin comp-host -- \
      --app graphviz --config-file ../examples/defaults.conf --config default-tenant=graphviz \
      --component ../{{graphviz_composed}} --addr 0.0.0.0:3056

compose-health-records: compose
	@just _derive health-records-domain {{health_records_composed}}

host-health-records: compose-health-records
	cd host && cargo run --release --bin comp-host -- \
	  --app health-records --config-file ../examples/defaults.conf --config default-tenant=health-records \
	  --component ../{{health_records_composed}} --addr 0.0.0.0:3055

e2e-health-records: compose-health-records
	cd host && cargo build --release --bin comp-host
	cd examples/health-records && cargo test --release

screencast-health-records: compose-health-records
	node tools/screencast/health-records.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/health-records/*.webm docs/media/health-records.gif 820 10

compose-freight-tracker: compose
	@just _derive freight-tracker-domain {{freight_tracker_composed}}

host-freight-tracker: compose-freight-tracker
	cd host && cargo run --release --bin comp-host -- \
	  --app freight-tracker --config-file ../examples/defaults.conf --config default-tenant=freight-tracker \
	  --component ../{{freight_tracker_composed}} --addr 0.0.0.0:3055

e2e-freight-tracker: compose-freight-tracker
	cd host && cargo build --release --bin comp-host
	cd examples/freight-tracker && cargo test --release

screencast-freight-tracker: compose-freight-tracker
	node tools/screencast/freight-tracker.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/freight-tracker/*.webm docs/media/freight-tracker.gif 820 10

compose-smart-home: compose
	@just _derive smart-home-domain {{smart_home_composed}}

host-smart-home: compose-smart-home
	cd host && cargo run --release --bin comp-host -- \
	  --app smart-home --config-file ../examples/defaults.conf --config default-tenant=smart-home \
	  --component ../{{smart_home_composed}} --addr 0.0.0.0:3055

e2e-smart-home: compose-smart-home
	cd host && cargo build --release --bin comp-host
	cd examples/smart-home && cargo test --release

screencast-smart-home: compose-smart-home
	node tools/screencast/smart-home.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/smart-home/*.webm docs/media/smart-home.gif 820 10

compose-academic-review: compose
	@just _derive academic-review-domain {{academic_review_composed}}

host-academic-review: compose-academic-review
	cd host && cargo run --release --bin comp-host -- \
	  --app academic-review --config-file ../examples/defaults.conf --config default-tenant=academic-review \
	  --component ../{{academic_review_composed}} --addr 0.0.0.0:3055

e2e-academic-review: compose-academic-review
	cd host && cargo build --release --bin comp-host
	cd examples/academic-review && cargo test --release

screencast-academic-review: compose-academic-review
	node tools/screencast/academic-review.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/academic-review/*.webm docs/media/academic-review.gif 820 10

compose-real-estate-escrow: compose
	@just _derive real-estate-escrow-domain {{real_estate_escrow_composed}}

host-real-estate-escrow: compose-real-estate-escrow
	cd host && cargo run --release --bin comp-host -- \
	  --app real-estate-escrow --config-file ../examples/defaults.conf --config default-tenant=real-estate-escrow \
	  --component ../{{real_estate_escrow_composed}} --addr 0.0.0.0:3055

e2e-real-estate-escrow: compose-real-estate-escrow
	cd host && cargo build --release --bin comp-host
	cd examples/real-estate-escrow && cargo test --release

screencast-real-estate-escrow: compose-real-estate-escrow
	node tools/screencast/real-estate-escrow.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/real-estate-escrow/*.webm docs/media/real-estate-escrow.gif 820 10

compose-iot-scanner: compose
	@just _derive iot-scanner {{iot_scanner_composed}}

compose-device-radar: compose-iot-scanner
	@just _derive device-radar-domain {{device_radar_composed}}

host-device-radar: compose-device-radar
	cd host && cargo run --release --bin comp-host -- \
	  --app device-radar --config-file ../examples/defaults.conf --config default-tenant=device-radar \
	  --component ../{{device_radar_composed}} --addr 0.0.0.0:3055

e2e-device-radar: compose-device-radar
	cd host && cargo build --release --bin comp-host
	cd examples/device-radar && cargo test --release

screencast-device-radar: compose-device-radar
	node tools/screencast/device-radar.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/device-radar/*.webm docs/media/device-radar.gif 820 10

compose-desktop-notifier:
    @echo "Linking desktop-notifier..."
    @wac plug components/target/wasm32-wasip2/release/desktop_notifier_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/desktop_ui_notifier.wasm \
        -o components/target/desktop-notifier.composed.wasm
    @echo "composed desktop-notifier -> components/target/desktop-notifier.composed.wasm"

host-desktop-notifier:
    cd host && cargo run --release --bin comp-host -- --app desktop-notifier --config-file ../examples/defaults.conf --config default-tenant=desktop-notifier --component ../components/target/desktop-notifier.composed.wasm --addr 0.0.0.0:3056

compose-clipboard-sync:
    @echo "Linking clipboard-sync..."
    @wac plug components/target/wasm32-wasip2/release/clipboard_sync_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/desktop_clipboard.wasm \
        -o components/target/clipboard-sync.composed.wasm
    @echo "composed clipboard-sync -> components/target/clipboard-sync.composed.wasm"

host-clipboard-sync:
    cd host && cargo run --release --bin comp-host -- --app clipboard-sync --config-file ../examples/defaults.conf --config default-tenant=clipboard-sync --component ../components/target/clipboard-sync.composed.wasm --addr 0.0.0.0:3056

compose-pdf-generator:
    @echo "Linking pdf-generator..."
    @wac plug components/target/wasm32-wasip2/release/pdf_generator_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/browser_automation.wasm \
        -o components/target/pdf-generator.composed.wasm
    @echo "composed pdf-generator -> components/target/pdf-generator.composed.wasm"

host-pdf-generator:
    cd host && cargo run --release --bin comp-host -- --app pdf-generator --config-file ../examples/defaults.conf --config default-tenant=pdf-generator --component ../components/target/pdf-generator.composed.wasm --addr 0.0.0.0:3056

compose-local-ai:
    @echo "Linking local-ai..."
    @wac plug components/target/wasm32-wasip2/release/local_ai_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/llm_local.wasm \
        -o components/target/local-ai.composed.wasm
    @echo "composed local-ai -> components/target/local-ai.composed.wasm"

host-local-ai:
    cd host && cargo run --release --bin comp-host -- --app local-ai --config-file ../examples/defaults.conf --config default-tenant=local-ai --component ../components/target/local-ai.composed.wasm --addr 0.0.0.0:3056

compose-docker-manager:
    @echo "Linking docker-manager..."
    @wac plug components/target/wasm32-wasip2/release/docker_manager_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/container_docker.wasm \
        -o components/target/docker-manager.composed.wasm
    @echo "composed docker-manager -> components/target/docker-manager.composed.wasm"

host-docker-manager:
    cd host && cargo run --release --bin comp-host -- --app docker-manager --config-file ../examples/defaults.conf --config default-tenant=docker-manager --component ../components/target/docker-manager.composed.wasm --addr 0.0.0.0:3056

compose-video-transcoder:
    @echo "Linking video-transcoder..."
    @wac plug components/target/wasm32-wasip2/release/video_transcoder_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/video_ffmpeg.wasm \
        -o components/target/video-transcoder.composed.wasm
    @echo "composed video-transcoder -> components/target/video-transcoder.composed.wasm"

host-video-transcoder:
    cd host && cargo run --release --bin comp-host -- --app video-transcoder --config-file ../examples/defaults.conf --config default-tenant=video-transcoder --component ../components/target/video-transcoder.composed.wasm --addr 0.0.0.0:3056

compose-lan-scanner:
    @echo "Linking lan-scanner..."
    @wac plug components/target/wasm32-wasip2/release/lan_scanner_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/lan_scanner.wasm \
        -o components/target/lan-scanner.composed.wasm
    @echo "composed lan-scanner -> components/target/lan-scanner.composed.wasm"

host-lan-scanner:
    cd host && cargo run --release --bin comp-host -- --app lan-scanner --config-file ../examples/defaults.conf --config default-tenant=lan-scanner --component ../components/target/lan-scanner.composed.wasm --addr 0.0.0.0:3056

compose-mdns-discoverer:
    @echo "Linking mdns-discoverer..."
    @wac plug components/target/wasm32-wasip2/release/mdns_discoverer_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/mdns_discovery.wasm \
        -o components/target/mdns-discoverer.composed.wasm
    @echo "composed mdns-discoverer -> components/target/mdns-discoverer.composed.wasm"

host-mdns-discoverer:
    cd host && cargo run --release --bin comp-host -- --app mdns-discoverer --config-file ../examples/defaults.conf --config default-tenant=mdns-discoverer --component ../components/target/mdns-discoverer.composed.wasm --addr 0.0.0.0:3056

compose-fs-watcher:
    @echo "Linking fs-watcher..."
    @wac plug components/target/wasm32-wasip2/release/fs_watcher_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/fs_watcher.wasm \
        -o components/target/fs-watcher.composed.wasm
    @echo "composed fs-watcher -> components/target/fs-watcher.composed.wasm"

host-fs-watcher:
    cd host && cargo run --release --bin comp-host -- --app fs-watcher --config-file ../examples/defaults.conf --config default-tenant=fs-watcher --component ../components/target/fs-watcher.composed.wasm --addr 0.0.0.0:3056

compose-vpn-manager:
    @echo "Linking vpn-manager..."
    @wac plug components/target/wasm32-wasip2/release/vpn_manager_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/vpn_wireguard.wasm \
        -o components/target/vpn-manager.composed.wasm
    @echo "composed vpn-manager -> components/target/vpn-manager.composed.wasm"

host-vpn-manager:
    cd host && cargo run --release --bin comp-host -- --app vpn-manager --config-file ../examples/defaults.conf --config default-tenant=vpn-manager --component ../components/target/vpn-manager.composed.wasm --addr 0.0.0.0:3056

compose-image-optimizer:
    @echo "Linking image-optimizer..."
    @wac plug components/target/wasm32-wasip2/release/image_optimizer_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/image_optimizer.wasm \
        -o components/target/image-optimizer.composed.wasm
    @echo "composed image-optimizer -> components/target/image-optimizer.composed.wasm"

host-image-optimizer:
    cd host && cargo run --release --bin comp-host -- --app image-optimizer --config-file ../examples/defaults.conf --config default-tenant=image-optimizer --component ../components/target/image-optimizer.composed.wasm --addr 0.0.0.0:3056

compose-cron-scheduler:
    @echo "Linking cron-scheduler..."
    @wac plug components/target/wasm32-wasip2/release/cron_scheduler_domain.wasm \
        --plug components/target/auth_guard.composed.wasm \
        --plug components/target/wasm32-wasip2/release/record_store.wasm \
        --plug components/target/wasm32-wasip2/release/system_cron.wasm \
        -o components/target/cron-scheduler.composed.wasm
    @echo "composed cron-scheduler -> components/target/cron-scheduler.composed.wasm"

host-cron-scheduler:
    cd host && cargo run --release --bin comp-host -- --app cron-scheduler --config-file ../examples/defaults.conf --config default-tenant=cron-scheduler --component ../components/target/cron-scheduler.composed.wasm --addr 0.0.0.0:3056
