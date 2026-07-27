# comp — WIT-first universal auth + RBAC. Task runner.
#
# Requires: wasm-tools, wkg, cargo-component, wac, docker compose.
# Runtime deploy additionally needs `wash` (wasmCloud host CLI, not bundled).

set dotenv-load := true

wit_dir := "wit"
components := "components"
rel := components / "target/wasm32-wasip1/release"
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
ai_composed := "components/target/ai_inference.composed.wasm"
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
mesh_wasm := rel / "mesh_domain.wasm"
mesh_composed := "components/target/mesh_domain.composed.wasm"
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

# Build all components to wasm components.
build:
    cd {{components}} && cargo component build --release

# Compose the rate-limiter AND audit-log into auth-guard with wac, satisfying
# auth-guard's `ratelimit:guard/limiter` + `audit:log/recorder` imports. Output
# is a single self-contained component.
compose: build
    wac plug {{guard_wasm}} --plug {{ratelimit_wasm}} --plug {{auditlog_wasm}} -o {{guard_composed}}
    @echo "composed auth-guard (+ rate-limiter + audit-log) -> {{guard_composed}}"

# Compose the vet-clinic DOMAIN component (the Rust HTTP backend) with every
# capability it imports: the composed auth-guard (auth:identity), records:store,
# validate:schema, search:index. Output is ONE self-contained app component that
# serves HTTP and runs identically on jco or a wasmCloud host — the whole
# vet-clinic backend as language-agnostic wasm, no Node.
compose-vet: compose
    wac plug {{vetdomain_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{validate_wasm}} \
      --plug {{searchindex_wasm}} \
      --plug {{staticassets_wasm}} \
      -o {{vet_composed}}
    @echo "composed vet-domain (+ auth-guard + records + validate + search + ui) -> {{vet_composed}}"

# Compose helpdesk-domain (HELPDESK.md rung 1) with every capability it
# imports: the composed auth-guard (auth:identity), records:store,
# fsm:workflow, id:generate, md:render. Remaining imports are generic WASI.
compose-helpdesk: compose
    wac plug {{helpdesk_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/fsm_workflow.wasm \
      --plug {{rel}}/id_generate.wasm \
      --plug {{rel}}/markdown.wasm \
      -o {{helpdesk_composed}}
    @echo "composed helpdesk-domain (+ auth-guard + records + fsm + ids + md) -> {{helpdesk_composed}}"

# Compose conduit-domain (CONDUIT.md rung 1 — the RealWorld spec) with the
# capabilities it imports: the composed auth-guard (auth:identity) + records:store.
# Remaining imports are generic WASI. Output is ONE self-contained app component.
compose-conduit: compose
    wac plug {{conduit_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/slug.wasm \
      -o {{conduit_composed}}
    @echo "composed conduit-domain (+ auth-guard + records + slug) -> {{conduit_composed}}"

# Run the conduit app (CONDUIT.md rung 1) on the native Rust host, in-memory KV.
host-conduit: compose-conduit
    cd host && VET_TENANT=conduit cargo run --release --bin vet-host -- \
      --component ../{{conduit_composed}} --addr 0.0.0.0:3008

# conduit e2e: build the composed app + native host, then a Rust test that spawns
# the host and drives the full API (users/profiles/articles/comments/favorites).
e2e-conduit: compose-conduit
    cd host && cargo build --release --bin vet-host
    cd examples/conduit && cargo test --release

# RealWorld conformance (CONDUIT.md rung 4): the OFFICIAL Hurl suite (vendored in
# examples/conduit/conformance/hurl) against the composed app on the native host.
# Requires `hurl` (https://hurl.dev) — like `wash`, not bundled.
conformance-conduit: compose-conduit
    cd host && cargo build --release --bin vet-host
    bash examples/conduit/conformance/run.sh

# Compose saga-domain (SAGA.md — a durable trip-booking saga) with the durable
# primitives it orchestrates: records + fsm + idempotency + event-bus + ids.
# No auth (anonymous engine). Remaining imports are generic WASI.
compose-saga: build
    wac plug {{saga_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/fsm_workflow.wasm \
      --plug {{idempotency_wasm}} \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      --plug {{rel}}/scheduler_timer.wasm \
      -o {{saga_composed}}
    @echo "composed saga-domain (+ records + fsm + idempotency + event-bus + ids + timer) -> {{saga_composed}}"

# Run the saga app on the native Rust host. Use --kv nats to prove durability
# (state survives a restart); memory is fine for the happy/compensation paths.
host-saga: compose-saga
    cd host && VET_TENANT=saga cargo run --release --bin vet-host -- \
      --component ../{{saga_composed}} --addr 0.0.0.0:3012

# Saga e2e: compose + build host + a Rust test that spawns the host and drives
# commit, compensation, and (NATS) resume-after-restart over real HTTP.
e2e-saga: compose-saga
    cd host && cargo build --release --bin vet-host
    cd examples/saga && cargo test --release

# Durability proof (SAGA.md rung 3): start a saga on NATS KV, advance it, KILL
# the host, restart, and show it resumes. Requires NATS on :4222.
durable-saga: compose-saga
    cd host && cargo build --release --bin vet-host
    bash examples/saga/durability.sh

# Golem provider (GOLEM.md): unit tests (contract + Value mapping + provider
# compiles). No infra — the live Golem hop skips without GOLEM_E2E.
golem-provider-test:
    cd providers/golem-workflow && cargo test --release

# Live e2e (GOLEM.md rung 3): download Golem 1.5, run it, deploy the demo agent,
# and invoke it through the provider's bridge (asserts durable state advances).
golem-e2e:
    bash providers/golem-workflow/e2e.sh

# Live proof (SAGA.md): a saga whose LEGS are real durable Golem workers. Starts
# Golem, deploys the agent, runs the saga with golem-backed legs over wasi:http,
# and asserts it committed with golem-issued refs + the worker's state advanced.
# Requires the Golem binary (run `just golem-e2e` once to fetch it).
saga-golem: compose-saga
    cd host && cargo build --release --bin vet-host
    bash examples/saga/golem-legs.sh

# Compose pulse-domain (REALTIME.md — a realtime chat room with SSE server-push)
# with records + event-bus + id-generate. No auth. Remaining imports are WASI.
compose-pulse: build
    wac plug {{pulse_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{pulse_composed}}
    @echo "composed pulse-domain (+ records + event-bus + ids) -> {{pulse_composed}}"

# Run the chat app on the native Rust host + serve the two-pane SPA. Open two
# browser windows on http://127.0.0.1:3015 and watch messages stream live.
host-pulse: compose-pulse
    cd host && VET_TENANT=pulse cargo run --release --bin vet-host -- \
      --component ../{{pulse_composed}} --addr 0.0.0.0:3015 \
      --static-dir ../examples/pulse/public

# Realtime e2e: compose + build host + a Rust test that posts a message and
# proves a SEPARATE held-open SSE connection receives it live.
e2e-pulse: compose-pulse
    cd host && cargo build --release --bin vet-host
    cd examples/pulse && cargo test --release

# Compose jobs-domain (JOBS.md — a durable background-job queue) with its
# capabilities: the outbox (durable queue), the IN-PROCESS durable:workflow
# backend (swap for the golem-workflow provider on a classic host), cron, the
# idempotency guard, and record-store. Remaining imports are WASI.
compose-jobs: build
    wac plug {{jobs_wasm}} \
      --plug {{rel}}/outbox.wasm \
      --plug {{rel}}/inproc_workflow.wasm \
      --plug {{rel}}/cron.wasm \
      --plug {{idempotency_wasm}} \
      --plug {{recordstore_wasm}} \
      -o {{jobs_composed}}
    @echo "composed jobs-domain (+ outbox + inproc-workflow + cron + idempotency + records) -> {{jobs_composed}}"

# Compose tempo-domain (TEMPO.md — a multi-person worktime logger) with the
# composed auth-guard (auth:identity) + records. Remaining imports are WASI.
compose-tempo: compose
    wac plug {{tempo_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{pdf_wasm}} \
      -o {{tempo_composed}}
    wasm-tools validate {{tempo_composed}}
    @echo "composed tempo-domain (+ auth-guard + records + pdf) -> {{tempo_composed}}"

# Run the worktime logger on the native host + serve the SPA. Open
# http://127.0.0.1:3040: register (pick admin to create projects/categories),
# log time or run a pomodoro timer, and see your charts; managers/admins see all.
# Build the React + shadcn SPA (Vite) to examples/tempo/dist.
build-tempo-ui:
    cd examples/tempo/ui && npm ci && npm run build

host-tempo: compose-tempo build-tempo-ui
    cd host && VET_TENANT=tempo cargo run --release --bin vet-host -- \
      --component ../{{tempo_composed}} --addr 0.0.0.0:3040 \
      --static-dir ../examples/tempo/dist

# Worktime e2e: compose + build host + a Rust test — admin creates projects +
# categories, members log entries + a pomodoro timer, and the report aggregates
# by project/category over a range with RBAC scope (member=own, manager=all).
e2e-tempo: compose-tempo
    cd host && cargo build --release --bin vet-host
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

# Compose booked-domain (BOOKED.md — a Calendly-lite booking service) with the
# composed auth-guard + records + lock-mutex (no double-book) + email-render
# (confirmation) + ical (.ics) + rrule (recurring). Remaining imports are WASI.
compose-booked: compose
    wac plug {{booked_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/lock_mutex.wasm \
      --plug {{rel}}/email_render.wasm \
      --plug {{rel}}/ical.wasm \
      --plug {{rel}}/rrule.wasm \
      -o {{booked_composed}}
    wasm-tools validate {{booked_composed}}
    @echo "composed booked-domain (+ auth-guard + records + lock + email + ical + rrule) -> {{booked_composed}}"

# Build the React + shadcn SPA (Vite) to examples/booked/dist.
build-booked-ui:
    cd examples/booked/ui && npm ci && npm run build

# Run the booking app on the native host + serve the SPA on :3041. Register as
# `owner` to create resources + weekly availability; anyone else books free
# slots (no double-book), gets an .ics + a confirmation.
host-booked: compose-booked build-booked-ui
    cd host && VET_TENANT=booked cargo run --release --bin vet-host -- \
      --component ../{{booked_composed}} --addr 0.0.0.0:3041 \
      --static-dir ../examples/booked/dist

# Booking e2e: owner creates a resource + availability; a member books a slot;
# a SECOND booking of the same slot is rejected (no double-book); concurrent
# attempts leave exactly one booking; a recurrence expands to N instances; and
# a booking exports to a valid .ics.
e2e-booked: compose-booked
    cd host && cargo build --release --bin vet-host
    cd examples/booked && cargo test --release

# Compose transit-domain (TRANSIT.md — a public-transport ticketing service)
# with auth-guard + records (single-use enforced by record-revision CAS) + qr
# (the scannable ticket). Remaining imports are WASI.
compose-transit: compose
    wac plug {{transit_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/qr.wasm \
      -o {{transit_composed}}
    wasm-tools validate {{transit_composed}}
    @echo "composed transit-domain (+ auth-guard + records + qr) -> {{transit_composed}}"

# Build the React + shadcn SPA (Vite) to examples/transit/dist.
build-transit-ui:
    cd examples/transit/ui && npm ci && npm run build

# Run the ticketing app on the native host + serve the SPA on :3042. Register as
# `rider` to buy fares (single / 60-min / 90-min / monthly) and show their QR;
# as `validator` to scan + validate with the device camera.
host-transit: compose-transit build-transit-ui
    cd host && VET_TENANT=transit cargo run --release --bin vet-host -- \
      --component ../{{transit_composed}} --addr 0.0.0.0:3042 \
      --static-dir ../examples/transit/dist

# Ticketing e2e: a rider buys tickets; a validator validates — a single is
# consumed by one scan (a second is rejected); a duration ticket activates with
# a remaining window; CONCURRENT scans of one single ticket accept exactly once;
# a fabricated code is rejected; and a ticket renders a valid QR SVG.
e2e-transit: compose-transit
    cd host && cargo build --release --bin vet-host
    cd examples/transit && cargo test --release

# Compose dashboards-domain (DASHBOARDS.md — personal metric dashboards) with
# auth-guard + records + svg-chart (server-side SVG chart rendering). Remaining
# imports are WASI.
compose-dashboards: compose
    wac plug {{dashboards_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/svg_chart.wasm \
      -o {{dashboards_composed}}
    wasm-tools validate {{dashboards_composed}}
    @echo "composed dashboards-domain (+ auth-guard + records + svg-chart) -> {{dashboards_composed}}"

# Build the React + shadcn SPA (Vite) to examples/dashboards/dist.
build-dashboards-ui:
    cd examples/dashboards/ui && npm ci && npm run build

# Run the dashboards app on the native host + serve the SPA on :3043. Register a
# new account (seeded with a demo dashboard); add panels and see them rendered to
# SVG charts on the server — the frontend has no charting library.
host-dashboards: compose-dashboards build-dashboards-ui
    cd host && VET_TENANT=dashboards cargo run --release --bin vet-host -- \
      --component ../{{dashboards_composed}} --addr 0.0.0.0:3043 \
      --static-dir ../examples/dashboards/dist

# Dashboards e2e: a fresh account is seeded a demo dashboard; each panel renders
# to a valid SVG per kind (bar/line/donut/sparkline); a new panel round-trips;
# and one account cannot read another's dashboards.
e2e-dashboards: compose-dashboards
    cd host && cargo build --release --bin vet-host
    cd examples/dashboards && cargo test --release

# Compose gate-domain (GATE.md — a durable traffic-shaping gateway) with records
# (the durable per-key state) + shaper (the token-bucket / GCRA math). The three
# patterns — rate limit, throttle, batch — are the Golem durable-worker model
# expressed over records:store revision CAS. Remaining imports are WASI.
compose-gate: compose
    wac plug {{gate_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/shaper.wasm \
      -o {{gate_composed}}
    wasm-tools validate {{gate_composed}}
    @echo "composed gate-domain (+ records + shaper) -> {{gate_composed}}"

# Build the React + shadcn SPA (Vite) to examples/gate/dist.
build-gate-ui:
    cd examples/gate/ui && npm ci && npm run build

# Run the gateway on the native host + serve the SPA on :3044. Fire bursts at the
# rate limiter (token bucket, 200/429), the throttle (GCRA smoothing), and submit
# items to watch a batch coalesce and flush — all per-key, durable state.
host-gate: compose-gate build-gate-ui
    cd host && VET_TENANT=gate cargo run --release --bin vet-host -- \
      --component ../{{gate_composed}} --addr 0.0.0.0:3044 \
      --static-dir ../examples/gate/dist

# Gateway e2e: a token bucket allows `capacity` then 429s then refills; GCRA
# admits a burst then spaces with an exact retry-after; concurrent hits on one
# key admit exactly `capacity` (durable per-key CAS = a single-writer worker);
# and a batch coalesces submits and flushes atomically with per-item results.
e2e-gate: compose-gate
    cd host && cargo build --release --bin vet-host
    cd examples/gate && cargo test --release

# Run gate as a REAL Golem agent (GATE.md) and prove EXACT serialization: a
# durable single-writer worker per key admits exactly `capacity` under a
# concurrent burst — where the shared-store gate-domain over-admits. Reuses the
# Golem 1.5 binary from the golem-workflow provider (fetch once via `golem-e2e`).
gate-golem:
    bash examples/gate/golem-run.sh

# Compose books-domain (BOOKS.md — double-entry bookkeeping) with auth-guard +
# records + ledger (the debits==credits invariant + trial balance) + pdf
# (statements). Remaining imports are WASI.
compose-books: compose
    wac plug {{books_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/ledger.wasm \
      --plug {{pdf_wasm}} \
      -o {{books_composed}}
    wasm-tools validate {{books_composed}}
    @echo "composed books-domain (+ auth-guard + records + ledger + pdf) -> {{books_composed}}"

# Build the React + shadcn SPA (Vite) to examples/books/dist.
build-books-ui:
    cd examples/books/ui && npm ci && npm run build

# Run the bookkeeping app on the native host + serve the SPA on :3045. Register
# a new account (seeded a demo chart + entries); post balanced journal entries
# and read the trial balance / P&L / balance sheet (+ PDF).
host-books: compose-books build-books-ui
    cd host && VET_TENANT=books cargo run --release --bin vet-host -- \
      --component ../{{books_composed}} --addr 0.0.0.0:3045 \
      --static-dir ../examples/books/dist

# Bookkeeping e2e: a balanced entry posts; an UNBALANCED entry is rejected; the
# trial balance's debits equal its credits; the balance sheet balances
# (assets = liabilities + equity + net income); and a statements PDF renders.
e2e-books: compose-books
    cd host && cargo build --release --bin vet-host
    cd examples/books && cargo test --release

# Compose stash-domain (STASH.md — a note stash you export as a .zip) with
# auth-guard + records + zip (the archive) + csv (the index inside it). Remaining
# imports are WASI.
compose-stash: compose
    wac plug {{stash_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/zip.wasm \
      --plug {{rel}}/csv.wasm \
      -o {{stash_composed}}
    wasm-tools validate {{stash_composed}}
    @echo "composed stash-domain (+ auth-guard + records + zip + csv) -> {{stash_composed}}"

# Build the React + shadcn SPA (Vite) to examples/stash/dist.
build-stash-ui:
    cd examples/stash/ui && npm ci && npm run build

# Run the note stash on the native host + serve the SPA on :3046. Register a new
# account (seeded demo notes), keep notes, and hit Export .zip to download them
# all as a real ZIP (Markdown + index.csv + manifest.json).
host-stash: compose-stash build-stash-ui
    cd host && VET_TENANT=stash cargo run --release --bin vet-host -- \
      --component ../{{stash_composed}} --addr 0.0.0.0:3046 \
      --static-dir ../examples/stash/dist

# Stash e2e: notes CRUD; the export is a valid ZIP (PK header + intact central
# directory) containing a .md per note, an index.csv, and a manifest.json.
e2e-stash: compose-stash
    cd host && cargo build --release --bin vet-host
    cd examples/stash && cargo test --release

# Compose payees-domain (PAYEES.md — a payee book) with auth-guard + records +
# iban (validate the IBAN before storing). Remaining imports are WASI.
compose-payees: compose
    wac plug {{payees_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/iban.wasm \
      -o {{payees_composed}}
    wasm-tools validate {{payees_composed}}
    @echo "composed payees-domain (+ auth-guard + records + iban) -> {{payees_composed}}"

# Build the React + shadcn SPA (Vite) to examples/payees/dist.
build-payees-ui:
    cd examples/payees/ui && npm ci && npm run build

# Run the payee book on the native host + serve the SPA on :3047. Register a new
# account (seeded demo payees); add payees — the IBAN is validated as you type
# (country length + mod-97 checksum) and a typo is refused with the reason.
host-payees: compose-payees build-payees-ui
    cd host && VET_TENANT=payees cargo run --release --bin vet-host -- \
      --component ../{{payees_composed}} --addr 0.0.0.0:3047 \
      --static-dir ../examples/payees/dist

# Payee-book e2e: a valid IBAN is accepted (stored normalized + country); a
# bad-checksum / wrong-length / bad-country IBAN is rejected with the reason;
# /verify returns the parsed country + grouped form; and ownership is enforced.
e2e-payees: compose-payees
    cd host && cargo build --release --bin vet-host
    cd examples/payees && cargo test --release

# Compose lms-domain (LMS.md — a learning platform) with auth-guard + records +
# quiz (auto-grade + stats) + pdf (certificate) + svg-chart (gradebook chart).
# Remaining imports are WASI.
compose-lms: compose
    wac plug {{lms_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/quiz_grade.wasm \
      --plug {{pdf_wasm}} \
      --plug {{rel}}/svg_chart.wasm \
      -o {{lms_composed}}
    wasm-tools validate {{lms_composed}}
    @echo "composed lms-domain (+ auth-guard + records + quiz + pdf + svg-chart) -> {{lms_composed}}"

# Build the React + shadcn SPA (Vite) to examples/lms/dist.
build-lms-ui:
    cd examples/lms/ui && npm ci && npm run build

# Run the learning platform on the native host + serve the SPA on :3048. Register
# as `instructor` (creates courses/lessons/quizzes; seeded a demo course) or as
# `student` (enroll, take auto-graded quizzes, see progress + certificate).
host-lms: compose-lms build-lms-ui
    cd host && VET_TENANT=lms cargo run --release --bin vet-host -- \
      --component ../{{lms_composed}} --addr 0.0.0.0:3048 \
      --static-dir ../examples/lms/dist

# Learning e2e: an instructor creates a course + quiz; a student enrolls and
# submits, which auto-grades (quiz:grade); the instructor gradebook reflects it
# consistently; a certificate issues once every quiz is passed; and the student's
# progress reconciles with the gradebook.
e2e-lms: compose-lms
    cd host && cargo build --release --bin vet-host
    cd examples/lms && cargo test --release

# Compose buzz-domain (BUZZ.md — a live multiplayer quiz game) with auth-guard +
# records. Remaining imports are WASI (random for the PIN, clocks for timing).
compose-buzz: compose
    wac plug {{buzz_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      -o {{buzz_composed}}
    wasm-tools validate {{buzz_composed}}
    @echo "composed buzz-domain (+ auth-guard + records) -> {{buzz_composed}}"

# Build the React + shadcn SPA (Vite) to examples/buzz/dist.
build-buzz-ui:
    cd examples/buzz/ui && npm ci && npm run build

# Run the quiz game on the native host + serve the SPA on :3049. Sign in as a
# host to run a game (get a PIN), or open on other devices to Join with the PIN
# and a nickname; the host drives the questions and everyone buzzes in.
host-buzz: compose-buzz build-buzz-ui
    cd host && VET_TENANT=buzz cargo run --release --bin vet-host -- \
      --component ../{{buzz_composed}} --addr 0.0.0.0:3049 \
      --static-dir ../examples/buzz/dist

# Game e2e: a host starts a game; two players join and answer at different speeds;
# reveal grades speed-weighted (faster-correct > slower-correct > wrong=0); the
# leaderboard ranks correctly; and the game ends on a podium.
e2e-buzz: compose-buzz
    cd host && cargo build --release --bin vet-host
    cd examples/buzz && cargo test --release

# Compose mesh-domain (MESH.md — resilient upstream calls) with records (the
# durable per-key circuit state) + resilience (the breaker state machine and the
# backoff schedule) + proxy-route (the REAL outgoing HTTP hop). Remaining imports
# are WASI: clocks for latency + the backoff sleep, config for the route table.
compose-mesh: compose
    wac plug {{mesh_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/resilience.wasm \
      --plug {{rel}}/proxy_route.wasm \
      -o {{mesh_composed}}
    wasm-tools validate {{mesh_composed}}
    @echo "composed mesh-domain (+ records + resilience + proxy-route) -> {{mesh_composed}}"

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
    cd ../../host && VET_TENANT=mesh \
      CFG_ROUTES='/upstream=http://127.0.0.1:3051/,/dead=http://127.0.0.1:3052/' \
      cargo run --release --bin vet-host -- \
      --component ../{{mesh_composed}} --addr 0.0.0.0:3050 \
      --static-dir ../examples/mesh/dist

# Resilience e2e against the REAL flaky upstream: retries ride out a two-request
# blip; `failure_threshold` failures trip the breaker and while it is OPEN the
# upstream's own hit counter proves it is never dialled; a half-open probe closes
# it again; a response slower than `slo_ms` counts as failed despite its 200; an
# unreachable upstream trips the breaker but a missing route (our config bug)
# does not.
e2e-mesh: compose-mesh
    cd host && cargo build --release --bin vet-host
    cd examples/mesh && cargo build --release --bin flaky && cargo test --release


# Build ONE self-contained image (vet-host + composed component + built SPA).
# No wasmCloud — vet-host serves http + the SPA + Redis-backed storage in one
# process. Run: docker run -p 8080:8080 -e REDIS_URL=rediss://... tempo
docker-tempo: compose-tempo build-tempo-ui
    docker build -f examples/tempo/Dockerfile -t tempo .
    @echo "built image 'tempo' — docker run -p 8080:8080 -e REDIS_URL=rediss://user:pw@host:25061 tempo"

# Compose arena-domain (ARENA.md — multiplayer Connect Four) with records +
# id-generate. Remaining imports are WASI.
compose-arena: build
    wac plug {{arena_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/id_generate.wasm \
      -o {{arena_composed}}
    @echo "composed arena-domain (+ records + ids) -> {{arena_composed}}"

# Run the game on the native host + serve the SPA. Open two windows on
# http://127.0.0.1:3039 — create a game in one, join from the other, play live.
host-arena: compose-arena
    cd host && VET_TENANT=arena cargo run --release --bin vet-host -- \
      --component ../{{arena_composed}} --addr 0.0.0.0:3039 \
      --static-dir ../examples/arena/public

# Game e2e: compose + build host + a Rust test that plays a full game — create,
# join, turn/seat/illegal-move rejection, a scripted win, and a concurrent-move
# revision conflict.
e2e-arena: compose-arena
    cd host && cargo build --release --bin vet-host
    cd examples/arena && cargo test --release

# Golem-backed variant: same queue, but durable:workflow is satisfied by the
# golem-bridge component (calls a durable Golem worker over wasi:http) instead of
# the in-process backend. The composed wasm now imports wasi:http/outgoing-handler
# (a host interface on the v2 operator). Point it at Golem via CFG_GOLEM_URL.
compose-jobs-golem: build
    wac plug {{jobs_wasm}} \
      --plug {{rel}}/outbox.wasm \
      --plug {{rel}}/golem_bridge.wasm \
      --plug {{rel}}/cron.wasm \
      --plug {{idempotency_wasm}} \
      --plug {{recordstore_wasm}} \
      -o components/target/jobs_domain.golem.wasm
    @echo "composed jobs-domain GOLEM variant (+ outbox + golem-bridge + cron + idempotency + records) -> components/target/jobs_domain.golem.wasm"

jobs_reg := env_var_or_default("JOBS_REG", "localhost:30501")

# Deploy the jobs queue on the wasmCloud v2 operator with the GOLEM backend live.
# Infra first (once): a v2 runtime-operator (bundles the host + NATS) in the
# `jobs` ns, same chart as eshop:
#   helm install jobs <wasmCloud>/charts/runtime-operator -n jobs \
#     -f examples/eshop/k8s/values.yaml   (chart v2.5.2)
# Golem must be reachable from pods (host.docker.internal:9006 on orbstack) with
# a `counters` demo agent deployed (providers/golem-workflow/e2e.sh). Then open
# http://jobs.jobs.svc.cluster.local
k8s-jobs: compose-jobs-golem
    kubectl apply -f examples/jobs/k8s/registry.yaml
    kubectl -n jobs rollout status deploy/registry --timeout=90s
    wkg oci push --insecure {{jobs_reg}} {{jobs_reg}}/jobs-domain-golem:0.1.0 components/target/jobs_domain.golem.wasm
    kubectl apply -f examples/jobs/k8s/jobs.yaml
    @echo "deployed jobs (golem backend) -> http://jobs.jobs.svc.cluster.local"

# Run the job queue on the native host + serve the board SPA. Open
# http://127.0.0.1:3038: enqueue jobs, watch them run/retry/dead-letter live,
# replay from the DLQ. CFG tunes the outbox: 3 attempts, 1s base backoff.
host-jobs: compose-jobs
    cd host && VET_TENANT=jobs CFG_MAX_ATTEMPTS=2 CFG_BASE_BACKOFF=1 \
      cargo run --release --bin vet-host -- \
      --component ../{{jobs_composed}} --addr 0.0.0.0:3038 \
      --static-dir ../examples/jobs/public

# Job-queue e2e: compose + build host + a Rust test that enqueues jobs, drives
# ticks, and proves success / retry-then-succeed / dead-letter / replay + the
# exactly-once enqueue key.
e2e-jobs: compose-jobs
    cd host && cargo build --release --bin vet-host
    cd examples/jobs && cargo test --release

# Compose scribe-domain (SCRIBE.md — a collaborative document editor) with the
# crdt merge component + records + id-generate. Remaining imports are WASI.
compose-scribe: build
    wac plug {{scribe_wasm}} \
      --plug {{rel}}/crdt.wasm \
      --plug {{rel}}/textdiff.wasm \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/id_generate.wasm \
      -o {{scribe_composed}}
    @echo "composed scribe-domain (+ crdt + textdiff + records + ids) -> {{scribe_composed}}"

# Run the collaborative editor on the native host + serve the two-pane SPA. Open
# two windows on http://127.0.0.1:3037 and edit the same doc — edits merge and
# stream live to both.
host-scribe: compose-scribe
    cd host && VET_TENANT=scribe cargo run --release --bin vet-host -- \
      --component ../{{scribe_composed}} --addr 0.0.0.0:3037 \
      --static-dir ../examples/scribe/public

# Collaborative-editor e2e: compose + build host + a Rust test proving two
# concurrent edits merge (both survive) and a live SSE connection sees them.
e2e-scribe: compose-scribe
    cd host && cargo build --release --bin vet-host
    cd examples/scribe && cargo test --release

# Compose pipeline-domain (PIPELINE.md — a reliable event pipeline with
# outbox → dispatch → DLQ → replay, SSE server-push) with outbox + event-bus +
# id-generate. No auth. Remaining imports are WASI (bound at deploy).
compose-pipeline: build
    wac plug {{pipeline_wasm}} \
      --plug {{rel}}/outbox.wasm \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{pipeline_composed}}
    @echo "composed pipeline-domain (+ outbox + event-bus + ids) -> {{pipeline_composed}}"

# Run the pipeline board on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3016: POST events, toggle the sink down, watch retries drop
# to the dead-letter tray, then Replay them — live over SSE.
host-pipeline: compose-pipeline
    cd host && VET_TENANT=pipeline cargo run --release --bin vet-host -- \
      --component ../{{pipeline_composed}} --addr 0.0.0.0:3016 \
      --static-dir ../examples/pipeline/public

# Reliability e2e: compose + build host + a Rust test that enqueues events,
# proves they deliver (acked), then takes the sink down and proves an event
# retries and drops to the dead-letter tray, and that Replay requeues it.
e2e-pipeline: compose-pipeline
    cd host && cargo build --release --bin vet-host
    cd examples/pipeline && cargo test --release

# Compose flags-domain (FLAGS.md — a live feature-rollout console with SSE
# server-push) with feature-flags + event-bus + id-generate. No auth. Remaining
# imports are WASI (kv + config bound at deploy).
compose-flags: build
    wac plug {{flags_wasm}} \
      --plug {{featureflags_wasm}} \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{flags_composed}}
    @echo "composed flags-domain (+ feature-flags + event-bus + ids) -> {{flags_composed}}"

# Run the rollout console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3017: drag a flag to 30% and watch ~30 of 100 subject tiles
# light up instantly and stay sticky; trip the kill-switch — all dark at once.
host-flags: compose-flags
    cd host && VET_TENANT=flags cargo run --release --bin vet-host -- \
      --component ../{{flags_composed}} --addr 0.0.0.0:3017 \
      --static-dir ../examples/flags/public

# Rollout e2e: compose + build host + a Rust test that sets a 30% rule and
# proves (a) a subject is STICKY across repeated evals, (b) raising the
# percentage never turns an already-on subject off, and (c) a rule flip made by
# one request reaches a SEPARATE held-open SSE connection live.
e2e-flags: compose-flags
    cd host && cargo build --release --bin vet-host
    cd examples/flags && cargo test --release

# Compose abtest-domain (EXPERIMENT.md — an A/B/n experiment console with SSE)
# with experiment-assign + metrics-collect + event-bus + id-generate. No auth.
# Remaining imports are WASI (kv + config bound at deploy).
compose-abtest: build
    wac plug {{abtest_wasm}} \
      --plug {{rel}}/experiment_assign.wasm \
      --plug {{rel}}/metrics_collect.wasm \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{abtest_composed}}
    @echo "composed abtest-domain (+ experiment + metrics + event-bus + ids) -> {{abtest_composed}}"

# Run the experiment console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3018: define control/variant-a/variant-b weights, watch 100
# subjects split into arms (sticky as weights shift), fire conversions, and see
# the per-arm conversion-rate bars pull apart live.
host-abtest: compose-abtest
    cd host && VET_TENANT=abtest cargo run --release --bin vet-host -- \
      --component ../{{abtest_composed}} --addr 0.0.0.0:3018 \
      --static-dir ../examples/abtest/public

# Experiment e2e: compose + build host + a Rust test that defines a 50/25/25
# experiment and proves (a) assignment is STICKY per subject, (b) two different
# subjects can land in different arms, (c) the ~50/25/25 split holds across a
# cohort, (d) conversions attribute to the right arm's rate, and (e) an outcome
# recorded by one request reaches a SEPARATE held-open SSE connection live.
e2e-abtest: compose-abtest
    cd host && cargo build --release --bin vet-host
    cd examples/abtest && cargo test --release

# Compose search-domain (SEARCH.md — faceted search-as-you-type) with the
# engine + corpus + cache (pre-composed with its kv backing) + metrics +
# pagination + ids. No auth. Remaining imports are WASI (kv + config).
compose-search: build
    wac plug {{rel}}/cache.wasm --plug {{rel}}/cache_backing.wasm -o components/target/cache.composed.wasm
    wac plug {{search_wasm}} \
      --plug {{searchindex_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug components/target/cache.composed.wasm \
      --plug {{rel}}/metrics_collect.wasm \
      --plug {{rel}}/pagination.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{search_composed}}
    @echo "composed search-domain (+ index + records + cache + metrics + paginate + ids) -> {{search_composed}}"

# Run the search console on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3019: type in the box, watch ranked hits narrow live, click a
# facet chip to filter, and watch the cache hit-ratio climb on repeat queries.
host-search: compose-search
    cd host && VET_TENANT=search cargo run --release --bin vet-host -- \
      --component ../{{search_composed}} --addr 0.0.0.0:3019 \
      --static-dir ../examples/search/public

# Search e2e: compose + build host + a Rust test that seeds the corpus and
# proves ranked results (a rare term ranks its doc first), all-mode intersection
# shrinks the set, a tag facet restricts hits, and the cache serves a repeat
# query (hit-ratio rises).
e2e-search: compose-search
    cd host && cargo build --release --bin vet-host
    cd examples/search && cargo test --release

# Compose throttle-domain (RATELIMIT.md — a live throttle wall) with the two
# limiters + event-bus + id-generate. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-ratelimit: build
    wac plug {{throttle_wasm}} \
      --plug {{ratelimit_wasm}} \
      --plug {{rel}}/quota.wasm \
      --plug {{rel}}/event_bus.wasm \
      --plug {{rel}}/id_generate.wasm \
      -o {{throttle_composed}}
    @echo "composed throttle-domain (+ ratelimit + quota + event-bus + ids) -> {{throttle_composed}}"

# Run the throttle wall on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3020: hold the hammer button, watch the attempt bar hit the
# ceiling and the key LOCK with a countdown, and the quota gauge drain — live.
# CFG_MAX_ATTEMPTS / CFG_LOCKOUT_WINDOW tune the wall (defaults 5 / 300s).
host-ratelimit: compose-ratelimit
    cd host && VET_TENANT=throttle CFG_MAX_ATTEMPTS=10 CFG_LOCKOUT_WINDOW=15 \
      cargo run --release --bin vet-host -- \
      --component ../{{throttle_composed}} --addr 0.0.0.0:3020 \
      --static-dir ../examples/ratelimit/public

# Throttle e2e: compose + build host + a Rust test that proves N allowed then a
# 429 at the ceiling, a quota `remaining` that decrements, lockout after a burst
# of failures, and a verdict reaching a SEPARATE held-open SSE connection.
e2e-ratelimit: compose-ratelimit
    cd host && cargo build --release --bin vet-host
    cd examples/ratelimit && cargo test --release

# Compose upload-drop (DROP.md — a presigned direct-upload drop-box) with the
# gate + blob store + signer + records + ids. No auth. Remaining imports are
# WASI (kv + config bound at deploy — see CFG_* below).
compose-drop: build
    wac plug {{drop_wasm}} \
      --plug {{rel}}/upload_policy.wasm \
      --plug {{rel}}/blob_store.wasm \
      --plug {{rel}}/webhook_sign.wasm \
      --plug {{recordstore_wasm}} \
      -o {{drop_composed}}
    @echo "composed upload-drop (+ upload-policy + blob-store + webhook-sign + records) -> {{drop_composed}}"

# Run the drop-box on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3021: pick a file, watch it ask for a ticket (the policy
# answer), PUT the bytes straight to storage, then get a signed download link.
# CFG_ALLOWED_TYPES / CFG_MAX_SIZE tune the gate (defaults: all types / 10 MiB).
host-drop: compose-drop
    cd host && VET_TENANT=drop \
      CFG_ALLOWED_TYPES=text/plain,image/png CFG_MAX_SIZE=1048576 \
      cargo run --release --bin vet-host -- \
      --component ../{{drop_composed}} --addr 0.0.0.0:3021 \
      --static-dir ../examples/drop/public

# Drop e2e: compose + build host + a Rust test that proves a ticket is minted
# for an allowed type, an oversized/blocked type is rejected at ticket time, a
# redeemed ticket stores bytes, and a signed download link round-trips the bytes
# while a tampered signature is refused.
e2e-drop: compose-drop
    cd host && cargo build --release --bin vet-host
    cd examples/drop && cargo test --release

# Compose csv-report (REPORT.md — batch CSV import/report) with the codec +
# validator + records + pagination. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-report: build
    wac plug {{report_wasm}} \
      --plug {{rel}}/csv.wasm \
      --plug {{rel}}/validate.wasm \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/pagination.wasm \
      -o {{report_composed}}
    @echo "composed csv-report (+ csv + validate + records + paginate) -> {{report_composed}}"

# Run the CSV import/report tool on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3022: paste a CSV, watch valid rows import and bad rows come
# back with per-field errors, page the clean report, then export it back to CSV.
host-report: compose-report
    cd host && VET_TENANT=report \
      cargo run --release --bin vet-host -- \
      --component ../{{report_composed}} --addr 0.0.0.0:3022 \
      --static-dir ../examples/report/public

# Report e2e: compose + build host + a Rust test that imports a CSV with a mix
# of valid + invalid rows (proving typed validation splits them with per-field
# errors), pages the clean set through the opaque cursor, and exports it back to
# CSV through the same codec (round-trip).
e2e-report: compose-report
    cd host && cargo build --release --bin vet-host
    cd examples/report && cargo test --release

# Compose mfa-authgate (AUTHGATE.md — TOTP 2FA + challenge-response login) with
# the otp primitive + secrets vault + session store + records. No auth-guard —
# this app IS the second factor. secrets:vault needs a 32-byte base64 master-key
# from config (CFG_MASTER_KEY below).
compose-authgate: build
    wac plug {{authgate_wasm}} \
      --plug {{rel}}/otp.wasm \
      --plug {{rel}}/qr.wasm \
      --plug {{secrets_wasm}} \
      --plug {{session_wasm}} \
      --plug {{recordstore_wasm}} \
      -o {{authgate_composed}}
    @echo "composed mfa-authgate (+ otp + qr + secrets-vault + session-store + records) -> {{authgate_composed}}"

# Run the 2FA authgate on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3023: enroll an account (scan the QR / copy the secret into an
# authenticator app), activate with the first code, then log in with a live code
# or burn a recovery code. CFG_MASTER_KEY seals the TOTP secret in the vault.
host-authgate: compose-authgate
    cd host && VET_TENANT=authgate \
      CFG_MASTER_KEY=bWZhLWRlbW8tbWFzdGVyLWtleS0zMi1ieXRlcyEhISE= \
      cargo run --release --bin vet-host -- \
      --component ../{{authgate_composed}} --addr 0.0.0.0:3023 \
      --static-dir ../examples/authgate/public

# Authgate e2e: compose + build host + a Rust test that provisions a secret,
# derives a valid TOTP code from it, activates enrollment, logs in with a live
# code (rejecting a wrong one), and burns a single-use recovery code (rejecting
# its reuse) — proving the full challenge-response lifecycle.
e2e-authgate: compose-authgate
    cd host && cargo build --release --bin vet-host
    cd examples/authgate && cargo test --release

# Compose paste-bin (PASTE.md — a paste/gist bin) with the pure-compute
# transform chain (validate + pii-redact + markdown + slug) plus the one
# stateful piece (records). No auth. Remaining imports are WASI (kv).
compose-paste: build
    wac plug {{paste_wasm}} \
      --plug {{validate_wasm}} \
      --plug {{rel}}/pii_redact.wasm \
      --plug {{rel}}/markdown.wasm \
      --plug {{rel}}/slug.wasm \
      --plug {{recordstore_wasm}} \
      -o {{paste_composed}}
    @echo "composed paste-bin (+ validate + pii-redact + markdown + slug + records) -> {{paste_composed}}"

# Run the paste bin on the native Rust host + serve the SPA. Open
# http://127.0.0.1:3024: paste Markdown (with an email or card number in it),
# submit, and watch the PII get masked at ingest and the Markdown render to safe
# HTML on view — a pure-compute pipeline with one stateful step.
host-paste: compose-paste
    cd host && VET_TENANT=paste \
      cargo run --release --bin vet-host -- \
      --component ../{{paste_composed}} --addr 0.0.0.0:3024 \
      --static-dir ../examples/paste/public

# Paste e2e: compose + build host + a Rust test that proves an empty body is
# rejected (validate), PII in the body is masked BEFORE storage (the raw email
# never appears in the stored/raw output), Markdown renders to sanitized HTML
# (a <script> is escaped, not executed), and duplicate titles get distinct
# slugs.
e2e-paste: compose-paste
    cd host && cargo build --release --bin vet-host
    cd examples/paste && cargo test --release

# Build the track SPA (Vite + TS) into components/track-assets/static, so the
# track-assets component's build.rs embeds it. Run before compose-track.
build-track-ui:
    cd examples/track/ui && npm install && npm run build

# Compose track-domain (TRACK.md — a Linear-lite project tracker) — the biggest
# composition in the repo: the pre-composed auth-guard + records + fsm + search +
# event-bus + notify + webhook-sign + policy + paginate + markdown + the
# pre-composed ai-inference (mock llm) + the baked SPA (track-assets). Five axes
# in one self-contained component. Depends on `compose` (guard), `compose-ai`
# (ai+mock-llm), and the built SPA.
compose-track: build-track-ui compose compose-ai
    wac plug {{track_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/fsm_workflow.wasm \
      --plug {{searchindex_wasm}} \
      --plug {{rel}}/event_bus.wasm \
      --plug {{notify_wasm}} \
      --plug {{rel}}/webhook_sign.wasm \
      --plug {{rel}}/policy_guard.wasm \
      --plug {{rel}}/pagination.wasm \
      --plug {{rel}}/markdown.wasm \
      --plug {{ai_composed}} \
      --plug {{trackassets_wasm}} \
      -o {{track_composed}}
    wasm-tools validate {{track_composed}}
    @echo "composed track-domain (+ auth-guard + records + fsm + search + bus + notify + websign + policy + paginate + md + ai + ui) -> {{track_composed}}"

# Run the project tracker on the native Rust host. The SPA is BAKED into the
# component (track-assets) — no --static-dir. Open http://127.0.0.1:3025:
# register (first user as admin), create a project, file issues, move them
# across the board, comment, watch the activity feed stream live over SSE, and
# summarize a thread with AI.
host-track: compose-track
    cd host && VET_TENANT=track \
      cargo run --release --bin vet-host -- \
      --component ../{{track_composed}} --addr 0.0.0.0:3025

# Track e2e: compose + build host + a Rust test driving all five axes — auth +
# RBAC (admin creates a project, a member writes, a non-member is 403), issue
# lifecycle over the fsm, full-text search, an SSE activity frame, the background
# stale-sweep tick, and the AI thread summary.
e2e-track: compose-track
    cd host && cargo build --release --bin vet-host
    cd examples/track && cargo test --release

# FULL-PARITY compose: plug every capability the parity vet-domain imports into
# one app component — all 19 (auth-guard, records, validate, search, blob,
# upload, fsm, money, markdown, csv, pii, otp, secrets, i18n, pagination,
# ai-inference (+mock llm, pre-composed), cache, timer, lock, event-bus). Output
# is the whole feature-complete vet-clinic backend as ONE wasm.
compose-vet-full: compose compose-ai
    # cache needs a backing store (source/sink); pre-compose cache + cache-backing
    # so the pair has zero non-WASI imports, then plug the pair.
    wac plug {{rel}}/cache.wasm --plug {{rel}}/cache_backing.wasm -o components/target/cache.composed.wasm
    wac plug {{vetdomain_wasm}} \
      --plug {{guard_composed}} \
      --plug {{ai_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{validate_wasm}} \
      --plug {{searchindex_wasm}} \
      --plug {{rel}}/blob_store.wasm \
      --plug {{rel}}/upload_policy.wasm \
      --plug {{rel}}/fsm_workflow.wasm \
      --plug {{rel}}/money.wasm \
      --plug {{rel}}/markdown.wasm \
      --plug {{rel}}/csv.wasm \
      --plug {{rel}}/pii_redact.wasm \
      --plug {{rel}}/otp.wasm \
      --plug {{rel}}/secrets_vault.wasm \
      --plug {{rel}}/i18n_catalog.wasm \
      --plug {{rel}}/pagination.wasm \
      --plug {{rel}}/scheduler_timer.wasm \
      --plug {{rel}}/lock_mutex.wasm \
      --plug {{rel}}/event_bus.wasm \
      --plug components/target/cache.composed.wasm \
      --plug {{staticassets_wasm}} \
      -o {{vet_full_composed}}
    @echo "composed FULL vet-domain (19 capabilities + ui) -> {{vet_full_composed}}"

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
    wac plug {{vetdomain_wasm}} \
      --plug {{rel}}/money.wasm \
      --plug {{validate_wasm}} \
      --plug {{rel}}/markdown.wasm \
      --plug {{rel}}/pii_redact.wasm \
      --plug {{rel}}/pagination.wasm \
      --plug {{rel}}/upload_policy.wasm \
      -o {{vet_lattice}}
    wasm-tools validate {{vet_lattice}}
    @echo "composed LATTICE vet-domain (+ 6 pure-compute caps fused, 28 core modules) -> {{vet_lattice}}"

# Run the composed vet-domain wasm under the NATIVE Rust host (wasmtime). No
# Node, no wasmCloud — `host/` is its own native binary that serves the
# component's HTTP and satisfies its keyvalue/config imports in-process.
host: compose-vet
    cd host && cargo run --release --bin vet-host -- --component ../{{vet_composed}} --addr 127.0.0.1:3007

# Run the FULL-PARITY app on the native host + serve the built React SPA. One
# Rust binary = UI + API. The whole vet-clinic, no Node. (--kv memory default.)
host-full: compose-vet-full
    cd host && cargo run --release --bin vet-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public

# Same, persisted to Redis (any redis-compatible server, e.g. valkey :6379).
host-redis: compose-vet-full
    cd host && cargo run --release --bin vet-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public \
      --kv redis --redis-url redis://127.0.0.1:6379

# Run the helpdesk app (HELPDESK.md rung 1) on the native host, persisted to
# NATS JetStream KV. Same bytes the jco example serves — different host.
host-helpdesk: compose-helpdesk
    cd host && VET_TENANT=helpdesk cargo run --release --bin vet-host -- \
      --component ../{{helpdesk_composed}} --addr 0.0.0.0:3007 \
      --static-dir ../examples/jco-helpdesk/public \
      --kv nats --nats-url 127.0.0.1:4222

# Same, persisted to NATS JetStream KV (:4222 by default).
host-nats: compose-vet-full
    cd host && cargo run --release --bin vet-host -- --component ../{{vet_full_composed}} \
      --addr 127.0.0.1:3007 --static-dir ../examples/jco-vet-clinic/public \
      --kv nats --nats-url 127.0.0.1:4222

# Compose the eshop-catalog service (ESHOP.md): eShopOnDapr's Catalog.API over
# record-store + event-bus + idempotency-guard (at-least-once dedup for the
# stock consumers). Output imports only generic WASI.
compose-eshop-catalog: build
    wac plug {{eshopcatalog_wasm}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/event_bus.wasm \
      --plug {{idempotency_wasm}} \
      -o {{eshopcatalog_composed}}
    @echo "composed eshop-catalog (+ records + event-bus + idempotency) -> {{eshopcatalog_composed}}"

# Compose every eshop service (ESHOP.md): eShopOnDapr recreated over comp
# contracts. identity = the existing accounts-app + composed auth-guard,
# untouched. Each output imports only generic WASI.
compose-eshop: compose compose-eshop-catalog
    wac plug {{rel}}/eshop_basket.wasm --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} --plug {{rel}}/event_bus.wasm -o {{eshopbasket_composed}}
    wac plug {{rel}}/eshop_ordering.wasm --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} --plug {{rel}}/fsm_workflow.wasm \
      --plug {{rel}}/event_bus.wasm --plug {{idempotency_wasm}} -o {{eshopordering_composed}}
    wac plug {{rel}}/eshop_payment.wasm --plug {{rel}}/event_bus.wasm -o {{eshoppayment_composed}}
    wac plug {{rel}}/accounts_app.wasm --plug {{guard_composed}} -o {{eshopidentity_composed}}
    wac plug {{rel}}/eshop_gateway.wasm --plug {{rel}}/proxy_route.wasm -o {{eshopgateway_composed}}
    wac plug {{rel}}/event_pusher.wasm --plug {{rel}}/proxy_route.wasm -o components/target/event_pusher.composed.wasm
    @echo "composed eshop services -> components/target/eshop_*.composed.wasm"

# Run the whole eshop (identity/catalog/basket/ordering/payment + gateway with
# the embedded storefront) on native hosts over a shared NATS at :4222.
# Gateway/storefront: http://127.0.0.1:3100 — smoke: examples/eshop/smoke.sh
host-eshop: compose-eshop
    examples/eshop/run-local.sh

eshop_reg := env_var_or_default("ESHOP_REG", "localhost:30500")

# Deploy eshop on wasmCloud v2 / k8s: push the six service images to the
# in-cluster registry (NodePort 30500) and apply the WorkloadDeployments.
# Infra first (once): helm install eshop <wasmCloud>/charts/runtime-operator \
#   -n eshop -f examples/eshop/k8s/values.yaml   (chart v2.5.2 verified)
# Then open http://gateway.eshop.svc.cluster.local (orbstack svc DNS).
k8s-eshop: compose-eshop
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-identity:0.1.1 {{eshopidentity_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-catalog:0.1.3 {{eshopcatalog_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-basket:0.1.2 {{eshopbasket_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-ordering:0.1.3 {{eshopordering_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-payment:0.1.2 {{eshoppayment_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/eshop-gateway:0.1.3 {{eshopgateway_composed}}
    wkg oci push --insecure {{eshop_reg}} {{eshop_reg}}/event-pusher:0.1.0 components/target/event_pusher.composed.wasm
    kubectl apply -f examples/eshop/k8s/registry.yaml -f examples/eshop/k8s/eshop.yaml

# Compose the idempotency-guard into webhook-ingest, satisfying its
# `idempotency:guard/store` import. Demonstrates one component composing another.
compose-webhook: build
    wac plug {{webhook_wasm}} --plug {{idempotency_wasm}} -o {{webhook_composed}}
    @echo "composed webhook-ingest (+ idempotency-guard) -> {{webhook_composed}}"

# Compose THREE capabilities — session:store + config:store + secrets:vault —
# into the login-app consumer, satisfying all three of its imports at once.
# The multi-capability composition demo: the output imports nothing but generic
# WASI host shims.
compose-login: build
    wac plug {{loginapp_wasm}} --plug {{session_wasm}} --plug {{config_wasm}} --plug {{secrets_wasm}} -o {{login_composed}}
    @echo "composed login-app (+ session + config + secrets) -> {{login_composed}}"

# Compose the link-shortener app: slug + id-generate + record-store +
# rate-limiter + cache (pre-composed with its kv backing). Output imports only
# generic WASI (keyvalue/clocks/random/config), so any comp host runs it.
compose-shortlink: build
    wac plug {{rel}}/cache.wasm --plug {{rel}}/cache_backing.wasm -o components/target/cache.composed.wasm
    wac plug {{shortlink_wasm}} \
      --plug {{rel}}/slug.wasm \
      --plug {{rel}}/id_generate.wasm \
      --plug {{recordstore_wasm}} \
      --plug {{ratelimit_wasm}} \
      --plug components/target/cache.composed.wasm \
      -o {{shortlink_composed}}
    wasm-tools validate {{shortlink_composed}}
    @echo "composed link-shortener (+ slug + id-generate + records + rate-limiter + cache) -> {{shortlink_composed}}"

# Run the composed link-shortener under the native host.
host-shortlink: compose-shortlink
    cd host && cargo run --release --bin vet-host -- --component ../{{shortlink_composed}} --addr 127.0.0.1:3008

# Compose the dev-portal app: the composed auth-guard (auth:identity) +
# record-store + id-generate + quota + policy-guard + outbox + webhook-sign +
# notify-dispatch. RBAC gates role verbs, policy-guard gates project access;
# key events leave as stripe-signed webhooks on an admin-pumped outbox drain.
compose-portal: compose
    wac plug {{portal_wasm}} \
      --plug {{guard_composed}} \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/id_generate.wasm \
      --plug {{rel}}/quota.wasm \
      --plug {{rel}}/policy_guard.wasm \
      --plug {{rel}}/outbox.wasm \
      --plug {{rel}}/webhook_sign.wasm \
      --plug {{rel}}/notify_dispatch.wasm \
      -o {{portal_composed}}
    wasm-tools validate {{portal_composed}}
    @echo "composed dev-portal (+ auth-guard + records + ids + quota + policy + outbox + sign + notify) -> {{portal_composed}}"

# Run the composed dev-portal under the native host.
host-portal: compose-portal
    cd host && cargo run --release --bin vet-host -- --component ../{{portal_composed}} --addr 127.0.0.1:3009

# Compose the webhook-relay app: the composed webhook-ingest (HMAC verify +
# replay dedup) + jsonpatch + outbox + webhook-sign + notify-dispatch +
# rate-limiter + audit-log + record-store. Ingest -> transform -> durable
# queue; drain delivers github-signed webhooks with retry + dead letters.
compose-relay: compose-webhook
    wac plug {{relay_wasm}} \
      --plug {{webhook_composed}} \
      --plug {{rel}}/jsonpatch.wasm \
      --plug {{rel}}/outbox.wasm \
      --plug {{rel}}/webhook_sign.wasm \
      --plug {{notify_wasm}} \
      --plug {{ratelimit_wasm}} \
      --plug {{auditlog_wasm}} \
      --plug {{recordstore_wasm}} \
      -o {{relay_composed}}
    wasm-tools validate {{relay_composed}}
    @echo "composed webhook-relay (+ ingest + jsonpatch + outbox + sign + notify + rate-limiter + audit + records) -> {{relay_composed}}"

# Run the composed webhook-relay under the native host.
host-relay: compose-relay
    cd host && cargo run --release --bin vet-host -- --component ../{{relay_composed}} --addr 127.0.0.1:3010

# Compose the billing-ledger app: money + record-store + idempotency-guard +
# quota + csv + outbox. Idempotency-key replay cache on the write path,
# integer minor-unit arithmetic, revision-CAS balances, csv statements.
compose-ledger: build
    wac plug {{ledger_wasm}} \
      --plug {{rel}}/money.wasm \
      --plug {{recordstore_wasm}} \
      --plug {{idempotency_wasm}} \
      --plug {{rel}}/quota.wasm \
      --plug {{rel}}/csv.wasm \
      --plug {{rel}}/outbox.wasm \
      -o {{ledger_composed}}
    wasm-tools validate {{ledger_composed}}
    @echo "composed billing-ledger (+ money + records + idempotency + quota + csv + outbox) -> {{ledger_composed}}"

# Run the composed billing-ledger under the native host.
host-ledger: compose-ledger
    cd host && cargo run --release --bin vet-host -- --component ../{{ledger_composed}} --addr 127.0.0.1:3011

# Compose the status-page app: scheduler-timer + record-store + fsm-workflow +
# event-bus + notify-dispatch. Timer-driven probes over outgoing HTTP; state
# transitions fan out on the bus and alert as webhooks.
compose-status: build
    wac plug {{statuspage_wasm}} \
      --plug {{rel}}/scheduler_timer.wasm \
      --plug {{recordstore_wasm}} \
      --plug {{rel}}/fsm_workflow.wasm \
      --plug {{rel}}/event_bus.wasm \
      --plug {{notify_wasm}} \
      -o {{statuspage_composed}}
    wasm-tools validate {{statuspage_composed}}
    @echo "composed status-page (+ timer + records + fsm + bus + notify) -> {{statuspage_composed}}"

# Run the composed status-page under the native host. Open
# http://127.0.0.1:3012: add a monitor (url + period >= 10s), then POST
# /api/tick to probe — the page shows each monitor's up/degraded/down state and
# its fsm transition history (up -> degraded -> down needs TWO failures).
host-status: compose-status
    cd host && cargo run --release --bin vet-host -- --component ../{{statuspage_composed}} --addr 127.0.0.1:3012

# Status-page e2e: compose + build host + a Rust test that adds a self-probe
# (stays up) and a dead-port monitor, then proves the fsm walks up -> degraded
# (one failure) -> down (a second consecutive failure), with both transitions in
# the history log. Slow: monitors have a 10s minimum period, so it sleeps across
# a period to force the second probe.
e2e-status: compose-status
    cd host && cargo build --release --bin vet-host
    cd examples/status && cargo test --release

# Compose an LLM provider into the ai-inference domain layer, satisfying its
# `llm:inference/inference` import. Here the deterministic MOCK provider is
# plugged in (for offline tests + demo); swap --plug for a real provider
# component (openai/anthropic/ollama) to go live — ai-inference is unchanged.
compose-ai: build
    wac plug {{rel}}/ai_inference.wasm --plug {{rel}}/llm_inference.wasm -o components/target/ai_inference.composed.wasm
    @echo "composed ai-inference (+ mock llm-inference) -> components/target/ai_inference.composed.wasm"

# Same domain layer, but with the REAL openai-provider plugged in instead of the
# mock — proves the swap is a composition choice, not a code change. The provider
# imports wasi:http + wasi:config (base-url / api-key / model), which the host
# (wasmCloud httpclient + config, or a jco http shim) satisfies at runtime.
compose-ai-openai: build
    wac plug {{rel}}/ai_inference.wasm --plug {{rel}}/openai_provider.wasm -o components/target/ai_inference.openai.composed.wasm
    @echo "composed ai-inference (+ openai-provider) -> components/target/ai_inference.openai.composed.wasm"

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
    @echo "composed login-app via wac source -> components/target/login_app.wac-composed.wasm"

# Validate the built components.
validate: build
    wasm-tools validate {{guard_wasm}}
    wasm-tools validate {{consumer_wasm}}
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
deploy-k8s:
    kubectl apply -f infra/k8s/host.yaml
    kubectl apply -f infra/k8s/app.yaml
    @just k8s-collapse

# Scale every host ReplicaSet except the newest to 0, so exactly one host runs.
# The operator can leave two RSes at 1 after a rollout, splitting the lattice
# (http provider and component land on different pods, wrpc calls fail). This
# makes a single co-located host deterministic — no manual `kubectl scale`.
k8s-collapse:
    #!/usr/bin/env bash
    set -euo pipefail
    sel="app.kubernetes.io/instance=comp-auth-host"
    live=$(kubectl get rs -n {{ns}} -l "$sel" \
      --sort-by='{.metadata.annotations.deployment\.kubernetes\.io/revision}' \
      -o custom-columns=N:.metadata.name,D:.spec.replicas --no-headers \
      | awk '$2>0{print $1}')
    n=$(echo "$live" | grep -c . || true)
    if [ "$n" -le 1 ]; then echo "single host already; nothing to collapse"; exit 0; fi
    # keep the last (newest revision), scale the rest to 0
    keep=$(echo "$live" | tail -1)
    for rs in $(echo "$live" | sed '$d'); do
      echo "scaling stale host RS $rs -> 0 (keeping $keep)"
      kubectl scale rs "$rs" -n {{ns}} --replicas=0
    done

# Full local check: vendor (if needed), validate WIT, build, validate components.
check: wit-check validate
    @echo "OK — contract resolves and both components build clean"
