# comp — WIT-first universal auth + RBAC. Task runner.
#
# Requires: wasm-tools, wkg, cargo-component, wac, docker compose.
# Runtime deploy additionally needs `wash` (wasmCloud host CLI, not bundled).

set dotenv-load := true

# Split by concern, and IMPORTED rather than made into modules. `import`
# splices a file in, so every recipe keeps the name the docs and the README
# already use; `mod` would have renamed all 201 of them (`just host build`),
# which is a worse trade than a long file.
#
# Verified by `just --dump`, which is the whole interface in one string: the
# split is right when that output does not change.
import 'just/compose.just'
import 'just/host.just'
import 'just/e2e.just'
import 'just/selfhost.just'

wit_dir := "wit"
components := "components"
rel := components / "target/wasm32-wasip2/release"
iot_scanner_composed := "components/target/iot-scanner.composed.wasm"
binder_composed := "components/target/binder-domain.composed.wasm"
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
events_wasm := rel / "events_domain.wasm"
events_composed := "components/target/events_domain.composed.wasm"
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
grocery_composed := "components/target/grocery_domain.composed.wasm"
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

# Fast, parallel test runner across all workspaces using cargo-nextest.
test-nextest:
    cargo xtask test

# Programmatic build runner (compilation, composition, fast testing) via cargo xtask.
xtask +args:
    cargo xtask {{args}}

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
    # Prune EVERY directory the graph reads, not just the one this builds into.
    # `comp-plug::default_dirs` names four — wasip2 and wasip1, release and debug —
    # and `cargo component check` fills the debug ones. Cleaning only
    # wasip2/release left a deleted component's artifact in three other places, and
    # `comp-capgraph` counted them: 212 components on a branch that has 208.
    pruned=0
    for stale in target/wasm32-wasip2/debug target/wasm32-wasip1/release target/wasm32-wasip1/debug; do
      [ -d "$stale" ] || continue
      for f in "$stale"/*.wasm; do
        [ -f "$f" ] || continue
        name=$(basename "$f" .wasm | tr '_' '-')
        if [ ! -f "$name/Cargo.toml" ]; then
          rm -f "$f"
          pruned=$((pruned+1))
        fi
      done
    done
    for f in target/wasm32-wasip2/release/*.wasm; do
      name=$(basename "$f" .wasm | tr '_' '-')
      stamp="$marker/$name"
      # A component that was deleted leaves its artifact behind — cargo removes
      # nothing it did not just write. Left alone it is indistinguishable from a
      # real one: it gets named, stamped, catalogued and drawn into the capability
      # graph, because everything downstream reads this directory rather than the
      # crate list.
      #
      # The CARGO.TOML is the check, not the directory. Switching to a branch
      # without a component leaves its directory behind whenever it holds an
      # ignored file — `src/bindings.rs` always does — so a directory test says
      # "still a crate" about a crate that is gone, and its surfaces reappear in
      # wit/SURFACES.md on a branch that never had it.
      if [ ! -f "$name/Cargo.toml" ]; then
        rm -f "$f" "$stamp"
        echo "pruned $name — components/$name/Cargo.toml is gone"
        pruned=$((pruned+1))
        continue
      fi
      if [ -z "{{force}}" ] && [ -f "$stamp" ] && [ ! "$f" -nt "$stamp" ]; then
        skipped=$((skipped+1)); continue
      fi
      wasm-tools metadata add --name "$name" --language "Rust=$rustv" "$f" -o "$f.named"
      mv "$f.named" "$f"
      touch "$stamp"
      stamped=$((stamped+1))
    done
    total=$((stamped + skipped))
    echo "built $total components (wasm32-wasip2, named, no preview1 adapter) — stamped $stamped, unchanged $skipped, pruned $pruned"

# Compose the rate-limiter AND audit-log into auth-guard with wac, satisfying
# auth-guard's `ratelimit:guard/limiter` + `audit:log/recorder` imports. Output
# is a single self-contained component.
# Compose ANY component with whatever it imports, derived rather than written.
#
# The 71 hand-written plug chains this replaced each named their plugs; this asks
# the component instead. There are none left — the last twelve were migrated once
# it turned out `comp-capgraph` could not see them at all. `reconciler/src/plug.rs` wraps `wac` as a library: read the
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
# Does a goal's contract document every capability its world imports?
#
#   just contract-critic                       # every goal
#   just contract-critic .comp/goals/x.toml    # one
#
# A part whose world imports `id:generate` but whose contract never quotes the call
# signature has to GUESS it, and a guess that compiles is the expensive kind of
# wrong. Deliberately NOT a CI test: it judges goal files, which are inputs to a
# loop that is paused, and gating a pull request on the prose of a historical goal
# is the wrong trade. It was unreachable, which is a different problem — the same one
# `tools/check-wit-packages.py` had, and that one turned out to be failing.
contract-critic *goals:
    @python3 tools/contract-critic.py {{ if goals == "" { ".comp/goals/*.toml" } else { goals } }}

# How much of a goal's app was reused rather than written (ADR-0089's whole claim).
#
#   just reuse-ratio .comp/goals/treasury-ledger.toml
#
# Three numbers from three sources so none can be talked up: the components
# `comp-plug` actually wired, the Rust lines in them against the lines in the goal's
# writable files, and what the artifact really imports. treasury-ledger reads 80.9%.
reuse-ratio +goals:
    @python3 tools/reuse-ratio.py {{goals}}

# `just capgraph json | jq` was parsing compiler output. The tool says what to do
# The component catalogue, from the components' own SOURCES.
#
#   just catalog
#
# Was `tools/gen-catalog.py`. Ported because the catalogue is load-bearing —
# `capsearch` reads it, and that is what stops a goal from generating a capability
# the pool already has — while having no test, because it could not have one: it
# embedded the last build's wasm size and hash and so was stale by construction.
# `docs/apps/STUDIO.md` has had "replace gen-catalog.py" on its list for a while.
catalog:
    @cd reconciler && cargo build --release --quiet --bin comp-catalog
    @./reconciler/target/release/comp-catalog

# when nothing is built.
capgraph format="md":
    @cd reconciler && cargo build --release --quiet --bin comp-capgraph
    @if [ "{{format}}" = "md" ]; then \
        ./reconciler/target/release/comp-capgraph --format md > docs/CAPABILITY-GRAPH.md; \
        echo "wrote docs/CAPABILITY-GRAPH.md"; \
     else \
        ./reconciler/target/release/comp-capgraph --format {{format}}; \
     fi

# BOTH committed derived files, in one command.
#
#   just derived
#
# There are two, not one, and that is worth a recipe because the count is the thing
# people get wrong. ADR-0097 and `reconciler/tests/derived.rs` both say
# `components/CATALOG.md` is "the one derived file still committed"; `just capgraph`
# writes a second one, `docs/CAPABILITY-GRAPH.md`, and it has its own staleness guard.
#
# Adding one component makes both stale. Two files, two commands, two tests, and the
# failure arrives one test at a time — regenerate the one the first failure names, push,
# and the other one fails next. That happened on the way to #201: `capgraph` was rerun,
# `catalog` was not, and CI caught it a commit later.
#
# Run this instead of either, after adding, renaming or re-describing a component.
derived: capgraph catalog

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

antigravity-shim port="8789" model="gemini-2.5-flash":
    @ANTIGRAVITY_MODEL="{{model}}" PORT="{{port}}" node tools/antigravity-shim.mjs

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








# RealWorld conformance (docs/apps/CONDUIT.md rung 4): the OFFICIAL Hurl suite (vendored in
# examples/conduit/conformance/hurl) against the composed app on the native host.
# Requires `hurl` (https://hurl.dev) — like `wash`, not bundled.
conformance-conduit: compose-conduit
    cd host && cargo build --release --bin comp-host
    bash examples/conduit/conformance/run.sh




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






# Run the worktime logger on the native host + serve the SPA. Open
# http://127.0.0.1:3040: register (pick admin to create projects/categories),
# log time or run a pomodoro timer, and see your charts; managers/admins see all.
# Build the React + shadcn SPA (Vite) to examples/tempo/dist.
build-tempo-ui:
    cd examples/tempo/ui && npm ci && npm run build



# Publish the composed tempo component to GHCR as a public OCI artifact — the
# wasmCloud-native pull path. `gh` mints the token, `wash` does the OCI push.
#
# THE TAG IS FOR PEOPLE; THE DIGEST IS WHAT YOU START. ADR-0006 allows a tag on a
# push and forbids one in anything that deploys — "tags drift and registries lie",
# and it names a live broken deploy caused by nothing else. This recipe used to
# print a `wash start … tempo:<version>` line, which is a deploy referencing a tag,
# so it told you to do the one thing the ADR exists to prevent.
#
# One-time setup:
#   gh auth refresh -s write:packages        # add the packages scope to gh
# After the FIRST push, make it public once: GitHub → your profile → Packages →
# tempo → Package settings → Visibility → Public (or "Connect repository").
push-tempo-ghcr version="0.1.0": compose-tempo
    #!/usr/bin/env bash
    set -euo pipefail
    ref="ghcr.io/{{ghcr_owner}}/tempo:{{version}}"
    out=$(wash oci push "$ref" {{tempo_composed}} \
      --user {{ghcr_owner}} --password "$(gh auth token)" -o json)
    printf '%s\n' "$out"
    # `wash` reports the digest it pushed; that is the only thing worth starting.
    digest=$(printf '%s' "$out" | python3 -c \
      'import sys,json,re; t=sys.stdin.read(); m=re.search(r"sha256:[0-9a-f]{64}", t); print(m.group(0) if m else "")')
    if [ -z "$digest" ]; then
      # Loudly, and without a fallback to the tag: a start line that names a tag is
      # the failure this recipe is meant to have stopped printing.
      echo "pushed $ref, but wash reported no digest — resolve it before deploying:" >&2
      echo "  curl -sI -H 'Accept: application/vnd.oci.image.manifest.v1+json' \\" >&2
      echo "    https://ghcr.io/v2/{{ghcr_owner}}/tempo/manifests/{{version}} | grep -i docker-content-digest" >&2
      exit 1
    fi
    echo "pushed $ref (set the package Public once)"
    echo "start it BY DIGEST — the tag can move, this cannot:"
    echo "  wash start component oci://ghcr.io/{{ghcr_owner}}/tempo@$digest tempo"


# Build the React + shadcn SPA (Vite) to examples/booked/dist.
build-booked-ui:
    cd examples/booked/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/transit/dist.
build-transit-ui:
    cd examples/transit/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/dashboards/dist.
build-dashboards-ui:
    cd examples/dashboards/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/gate/dist.
build-gate-ui:
    cd examples/gate/ui && npm ci && npm run build



# Run gate as a REAL Golem agent (docs/apps/GATE.md) and prove EXACT serialization: a
# durable single-writer worker per key admits exactly `capacity` under a
# concurrent burst — where the shared-store gate-domain over-admits. Reuses the
# Golem 1.5 binary from the golem-workflow provider (fetch once via `golem-e2e`).
gate-golem:
    bash examples/gate/golem-run.sh


# Build the React + shadcn SPA (Vite) to examples/books/dist.
build-books-ui:
    cd examples/books/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/stash/dist.
build-stash-ui:
    cd examples/stash/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/payees/dist.
build-payees-ui:
    cd examples/payees/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/lms/dist.
build-lms-ui:
    cd examples/lms/ui && npm ci && npm run build




# Build the React + shadcn SPA (Vite) to examples/buzz/dist.
build-buzz-ui:
    cd examples/buzz/ui && npm ci && npm run build






screencast-photosocial: compose-photosocial
    node tools/screencast/photosocial.mjs
    bash tools/screencast/to-gif.sh tools/screencast/videos/photosocial/*.webm docs/media/photosocial.gif 820 10


# Build the React + shadcn SPA (Vite) to examples/mesh/dist.
build-mesh-ui:
    cd examples/mesh/ui && npm ci && npm run build

# The deliberately flaky upstream mesh protects callers from (std-only, ~100
# lines). Fails on demand per request: /hit?fail=1, ?fail_n=2&id=x, ?delay=400.
# `host-mesh` starts it for you; run it alone to keep it up across host restarts.
mesh-upstream:
    cd examples/mesh && cargo run --release --bin flaky -- 127.0.0.1:3051




# Build the React + shadcn SPA (Vite) to examples/passkey/dist.
build-passkey-ui:
    cd examples/passkey/ui && npm ci && npm run build




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



# Build the native reconciler — the only process holding a lattice credential.
build-reconciler:
    cd reconciler && cargo build --release


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



# Build ONE self-contained image (comp-host + composed component + built SPA).
# No wasmCloud — comp-host serves http + the SPA + Redis-backed storage in one
# process. Run: docker run -p 8080:8080 -e REDIS_URL=rediss://... tempo
docker-tempo: compose-tempo build-tempo-ui
    docker build -f examples/tempo/Dockerfile -t tempo .
    @echo "built image 'tempo' — docker run -p 8080:8080 -e REDIS_URL=rediss://user:pw@host:25061 tempo"



































# MailHog on its usual ports, for watching mail arrive by eye at :8025 while you
# poke the app. The GATE does not need this — it starts its own on free ports.
mailhog:
    @echo "MailHog: SMTP 127.0.0.1:1025, UI http://127.0.0.1:8025"
    ~/go/bin/MailHog -smtp-bind-addr 127.0.0.1:1025 -api-bind-addr 127.0.0.1:8025 -ui-bind-addr 127.0.0.1:8025

# The bridge in front of it: HTTP in, real SMTP out.
mail-relay:
    cd reconciler && cargo build --release --bin comp-mailrelay
    ./reconciler/target/release/comp-mailrelay 127.0.0.1:3390 127.0.0.1:1025


# Build the React SPA to examples/events/dist. One page, two roles: `?as=attendee`
# and `?as=organizer` — which is what lets a recording show both at once.
build-events-ui:
    cd examples/events/ui && npm ci && npm run build


# The 24-hour reminder, three panes, with a REAL mailbox in the third.
# Prereq: `just mailhog &`, `just mail-relay &`, and a host started with
# --config mail:gateway-url=http://127.0.0.1:3390/ --egress 127.0.0.1:3390
screencast-events-reminder:
    node tools/screencast/events-reminder.mjs
    bash tools/screencast/to-gif.sh tools/screencast/videos/events-reminder/*.webm docs/media/events-reminder.gif 1000 10 3

# Both users, side by side, on the real app. Prereq: `just host-events &`
screencast-events:
    node tools/screencast/events.mjs
    bash tools/screencast/to-gif.sh tools/screencast/videos/events/*.webm docs/media/events.gif 900 10 2




# Build the track SPA (Vite + TS) into components/track-assets/static, so the
# track-assets component's build.rs embeds it. Run before compose-track.
build-track-ui:
    cd examples/track/ui && npm install && npm run build

# Build the grocery SPA (Vite + React + TS) into components/grocery-assets/static
build-grocery-ui:
    cd examples/grocery/ui && ([ -d node_modules ] || npm install) && npm run build



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


# Build the React + Vite SPA (router, recharts) to examples/binder/dist.
build-binder-ui:
    cd examples/binder/ui && npm ci && npm run build


# The camera works with NO key in the tenant, because it goes through the shim.
#
# `host-binder` points `vision:base-url` at `tools/claude-shim.mjs`, which speaks the
# same `/v1/messages` subset (images included) and runs on a subscription. So start
# the shim first and the photo button works:
#
#     node tools/claude-shim.mjs &
#     just host-binder
#
# Why this and not a granted secret: a single `comp-host` has no platform to fetch one
# FROM (ADR-0051), and more to the point a tenant should not be holding an API key at
# all. `components/anthropic-vision` requires one only when it is pointed at
# `anthropic.com` — the interface a guest sees is identical either way, which is what
# makes it a deploy-time choice.
#
# To use the metered API instead, point it back and grant the secret through a
# deployment (`fixtures/photo-critic.yaml` is the shape):
#
#     --config vision:base-url=https://api.anthropic.com --egress api.anthropic.com





























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




# ---- continuous deployment ---------------------------------------------------
#
# Two halves that never talk to each other. `.github/workflows/publish-apps.yml`
# composes every app, pushes it to ghcr BY DIGEST, and records which digest is
# current on the `deploy` branch. `comp-agent` runs here, reads that lock, and
# updates the apps this box already has a unit for.
#
# Nothing reaches in: no inbound port, no webhook, and no credential in a CI system
# that can touch this network. The only mutable thing in the chain is which commit
# of `apps.lock` is current — a branch in git, whose history IS the deploy history.





# ---- the shared services a tailnet box carries ------------------------------
#
# SurrealDB holds two things with different lifecycles in one schema (ADR-0091):
# the DERIVED capability graph, which `just capgraph-store` recomputes and can
# always throw away, and the ACCUMULATED pool of what runs have learned, which is
# the one thing in this system that cannot be recomputed.
#
# NATS is NOT installed here. The test harness spawns its own on a random loopback
# port for every run and must keep doing so — forty suites depend on that isolation
# — and a box that already has a JetStream NATS does not need a second. Point
# `NATS_URL` at the one that is there; `comp.<v>.<lattice>.…` subjects mean two
# lattices on one NATS never see each other's traffic.











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

# Stage every `.wasm` a jco example transpiles, from what `just build` produced.
#
# 58 of these were TRACKED, and all 50 with a same-named component had drifted from
# it — not by a metadata stamp, by thousands of bytes. `crdt.wasm` was 4 100 bytes
# larger than the component it was copied from. So ~40 examples were exercising
# frozen components that no longer exist anywhere else in the repository, and a
# green example said nothing about the component it claimed to demonstrate.
#
# `.gitignore` has carried `**/*.wasm` for a long time; those files predate the rule
# and git keeps tracking what it already tracks. The intent was recorded, the rule
# was written, the cleanup was not finished.
#
# Derived from each example's own `package.json` rather than listed here — the same
# reasoning `bench-setup` already gives for its 34: a hand-kept list of 58 is wrong
# the first time somebody adds an example, and wrong silently.
#
#   just examples-stage
examples-stage: build compose
    #!/usr/bin/env bash
    set -euo pipefail
    R=components/target/wasm32-wasip2/release

    # Three examples ask for a bare name and need the COMPOSED artifact, because a
    # bare component leaves non-WASI imports for jco to emit as bare specifiers,
    # which Node rejects outright as a URL scheme (`protocol 'audit:'`). auth-guard
    # imports ratelimit:guard + audit:log/recorder, and both it and audit-log import
    # audit:log/types — a TYPES-ONLY interface nothing exports, so composition cannot
    # satisfy it either; that one is stubbed at transpile time by the shims the
    # package.json files point at.
    #
    # The other three are components whose crate name is not the name the example
    # uses. Six entries, and every one of them is a fact about a specific example —
    # which is why they are a table and the other 55 are a rule.
    composed_alias() { case "$1" in
        audit_log|auth_guard|webhook_ingest) return 0 ;; *) return 1 ;; esac; }
    # An example that demonstrates a component in-process needs a DETERMINISTIC,
    # offline composition. Where the interface has several exporters, say which —
    # otherwise the pick is alphabetical and moves whenever somebody adds one.
    prefer() { case "$1" in
        ai_inference) echo llm_inference ;; *) echo "" ;; esac; }
    bare_alias() { case "$1" in
        eventbus) echo event_bus ;;
        lock)     echo lock_mutex ;;
        timer)    echo scheduler_timer ;;
        *)        echo "${1//-/_}" ;; esac; }

    staged=0
    for pj in examples/*/package.json; do
      dir=$(dirname "$pj")
      for want in $(grep -o 'jco transpile [^ "]*\.wasm' "$pj" | awk '{print $3}' | sort -u); do
        stem="${want%.wasm}"
        if [ "${stem%.composed}" != "$stem" ] || composed_alias "$stem"; then
          # `_derive` composes a component against its own imports, so the component
          # name is the artifact name with the suffix off and underscores hyphenated.
          base="${stem%.composed}"
          out="components/target/${base}.composed.wasm"
          if [ -n "$(prefer "$base")" ]; then
            # comp-plug picks ONE exporter per imported interface, and adding a
            # component can silently change which. `llm:inference/inference` now has
            # four exporters and the pick moved to `anthropic-provider` — so this
            # composition went from self-contained to needing a network key and a
            # secret, and jco-ai failed with an unresolvable `comp:secrets/reader`.
            # `--dir` wins earlier, so a directory holding one artifact pins it.
            pin=$(mktemp -d)
            cp "$R/$(prefer "$base").wasm" "$pin/"
            cp "$(./reconciler/target/release/comp-plug --dir "$pin" "${base//_/-}")" "$out"
            rm -rf "$pin"
          else
            just _derive "${base//_/-}" "$out" >/dev/null
          fi
          cp "$out" "$dir/$want"
        else
          cp "$R/$(bare_alias "$stem").wasm" "$dir/$want"
        fi
        staged=$((staged+1))
      done
    done
    echo "staged $staged example input(s) from the build — none of them tracked"

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
    just examples-stage
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
    # The shared pool is what makes the goals aware of each other's work — and it
    # needs the password to be a pool rather than a reported drop. This passed the
    # url alone, so every goal this recipe drained wrote its lessons nowhere.
    [ -n "${SURREAL_URL:-}" ] && run+=(--surreal-url "$SURREAL_URL")
    sp="${SURREAL_PASSWORD:-${SURREAL_PASSWORD_FILE:-}}"
    [ -n "$sp" ] && run+=(--surreal-password "${sp/#\~/$HOME}")
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
    # Both names, for the same reason as the timeout below. The flag takes a PATH
    # and `.comp/csatapaci.env` calls it SURREAL_PASSWORD_FILE — saying so, because
    # a password does not belong in argv — while this read SURREAL_PASSWORD. So
    # sourcing that file and running a goal passed `--surreal-url` and no password,
    # which against a root-auth database is a trace and a pool that write nothing.
    # It fails the way an absent record always does here: the run reports its drops
    # and carries on green.
    sp="${SURREAL_PASSWORD:-${SURREAL_PASSWORD_FILE:-}}"
    [ -n "$sp" ] && args+=(--surreal-password "${sp/#\~/$HOME}")
    # One knob, both names. The default is 900 and that is right for the API; a
    # local model is an order of magnitude slower (measured on csatapaci: 417s for
    # one branch-shaped call, so two attempts is 834s) and `.comp/csatapaci.env`
    # says GOAL_TIMEOUT=1800 for exactly that reason. `goald` read GOAL_TIMEOUT and
    # this read TIMEOUT, so the documented shim path — source the env file, then
    # `just goal-run` — silently ran at 900 and lost branches to it.
    t="${TIMEOUT:-${GOAL_TIMEOUT:-}}"
    [ -n "$t" ] && args+=(--timeout "$t")
    # A measured run pins this to 1.0 — never skip. At the default 0.9 a re-run after
    # a HARNESS failure is skipped as work already done, which reads as a pass.
    [ -n "${SKIP_ABOVE:-}" ] && args+=(--skip-above "$SKIP_ABOVE")
    [ "${DRY_RUN:-0}" = "1" ] && args+=(--dry-run)
    [ "${SMOKE:-0}" = "1" ] && args+=(--smoke)
    ./reconciler/target/release/comp-goalrun "${args[@]}"

graphviz_composed := "components/target/graphviz_domain.composed.wasm"






screencast-health-records: compose-health-records
	node tools/screencast/health-records.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/health-records/*.webm docs/media/health-records.gif 820 10




screencast-freight-tracker: compose-freight-tracker
	node tools/screencast/freight-tracker.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/freight-tracker/*.webm docs/media/freight-tracker.gif 820 10




screencast-smart-home: compose-smart-home
	node tools/screencast/smart-home.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/smart-home/*.webm docs/media/smart-home.gif 820 10




screencast-academic-review: compose-academic-review
	node tools/screencast/academic-review.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/academic-review/*.webm docs/media/academic-review.gif 820 10




screencast-real-estate-escrow: compose-real-estate-escrow
	node tools/screencast/real-estate-escrow.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/real-estate-escrow/*.webm docs/media/real-estate-escrow.gif 820 10





screencast-device-radar: compose-device-radar
	node tools/screencast/device-radar.mjs
	bash tools/screencast/to-gif.sh tools/screencast/videos/device-radar/*.webm docs/media/device-radar.gif 820 10

























# Everything CI runs, in the same order, with the same commands.
#
# This exists because it did not, and the gap cost three red pull requests. The
# habit was to run a plausible subset — `--test docs --test contracts` — and call
# the tree green. CI runs `--test publish` too, which is where a compose race lived.
#
# Worse, the local run was not even the same code path: `tests/harness/mod.rs`
# prefers `components/target/platform_domain.composed.wasm` when it exists, and it
# existed here and does not in a fresh checkout — so the compose that raced was
# never reached locally. `PLATFORM_COMPOSED=skip` below moves it aside for the run.
#
# Mirrors .github/workflows/ci.yml. When a job changes there, change it here.
ci-local:
    #!/usr/bin/env bash
    set -uo pipefail
    cd "{{justfile_directory()}}"
    fail=0
    step() { echo; echo "=== $* ==="; }

    # The stray composed artifact hides the cold path a fresh checkout takes.
    stray="components/target/platform_domain.composed.wasm"
    if [ -f "$stray" ]; then mv "$stray" "$stray.aside"; restore_stray=1; fi
    trap '[ -n "${restore_stray:-}" ] && mv "$stray.aside" "$stray"' EXIT

    step "components: just build force=1"
    just build force=1 || fail=1

    step "components: cargo test --workspace --exclude browser-automation"
    (cd components && cargo test --workspace --exclude browser-automation) || fail=1

    step "clippy: cargo component check + clippy --target wasm32-wasip2"
    (cd components && cargo component check --release) || fail=1
    (cd components && cargo clippy --workspace --exclude browser-automation --target wasm32-wasip2) || fail=1

    step "host: cargo build --release --bin comp-host"
    (cd host && cargo build --release --bin comp-host) || fail=1

    # The four suites the components job runs, on a COLD compose cache — which is
    # what CI has and what found the race.
    step "reconciler (components job): eight suites, on a cold compose cache"
    # Same reason as ci.yml's fetch step: the surfaces guard compares against the
    # base branch, and a checkout that has never fetched main has nothing to
    # compare against — it would skip here and fail in CI, which is the worst
    # order to discover it in.
    git fetch --depth=1 -q origin main:refs/remotes/origin/main 2>/dev/null || true
    rm -rf components/target/composed
    (cd reconciler && cargo test --release --test capsearch --test contracts --test publish --test secrets \
        --test capgraph_edges --test capgraph_store --test console_session --test compose_race \
        --test witsurface --test hostsurface --test fixtureversions) || fail=1

    step "reconciler (native job): --lib --bins + docs, fixtures, guestio, stress, uideps"
    (cd reconciler && cargo test --release --lib --bins \
        --test docs --test fixtures --test guestio \
        --test stress_env --test stress_tree --test uideps) || fail=1

    step "workflows: actionlint"
    if command -v actionlint >/dev/null; then actionlint || fail=1
    elif [ -x "$(go env GOPATH 2>/dev/null)/bin/actionlint" ]; then "$(go env GOPATH)/bin/actionlint" || fail=1
    else echo "actionlint not installed — CI will still lint. go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12"; fi

    echo
    if [ "$fail" -eq 0 ]; then echo "ci-local: everything CI runs is green here"; else echo "ci-local: FAILED"; fi
    exit "$fail"



# Fetch ONE component from a registry, by name or by digest.
#
#     just pull portfolio-value-c
#     just pull portfolio-value-c@sha256:…
#     just pull price-history-py /tmp/ph.wasm      # second arg is the output path
#
# Why this exists: building every component now means five toolchains, one of them
# a 200 MB wasi-sdk (docs/POLYGLOT.md), and `just fetch-components` reads GitHub
# Actions artifacts — which expire after thirty days, need a green run for that
# exact commit, and arrive as all 205 or none. This gets one, from bytes that do
# not expire, and verifies the digest before writing the file.
#
# Anonymous by default. Set OCI_USER/OCI_PASSWORD for a private registry.
pull name out="": (_cargo_bin "comp-oci")
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    out="{{out}}"
    [ -n "$out" ] || out="{{rel}}/$(echo "{{name}}" | sed 's/@.*//; s/:.*//' | tr - _).wasm"
    ./reconciler/target/release/comp-oci pull "${OCI_REGISTRY:-ghcr.io/{{ghcr_owner}}/holon}" "{{name}}" -o "$out"

# Push every built component to a registry, by digest. Needs OCI_USER/OCI_PASSWORD.
#
# CI does this from `.github/workflows/publish-components.yml`; this is the same
# command for a local registry or a dry run against your own namespace.
push-components registry="": (_cargo_bin "comp-oci")
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    reg="{{registry}}"
    [ -n "$reg" ] || reg="${OCI_REGISTRY:-ghcr.io/{{ghcr_owner}}/holon}"
    ./reconciler/target/release/comp-oci push "$reg" "{{rel}}" --lock components.lock

_cargo_bin bin:
    @cd reconciler && cargo build --release --quiet --bin {{bin}}

# Regenerate wit/SURFACES.md — every WIT package this repository defines, as its
# built components render it.
#
# The file is committed because the DIFF is the review: a pull request that changes
# a contract shows the reader exactly what moved, next to the version that did or
# did not move with it. `reconciler/tests/witsurface.rs` fails when a shape changed
# and its version did not.
wit-surfaces: build
    @cd reconciler && WIT_SURFACES=write cargo test --release --test witsurface \
        the_committed_surfaces_are_not_stale -- --nocapture 2>&1 | grep -E "wrote|SKIPPED" || true
    @echo "wrote wit/SURFACES.md"
