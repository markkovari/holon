# --------------------------------------------------------------------------
# Composing an app from its parts.
#
# Every recipe here is one `_derive` call or a small chain around one: `wac` is
# handed the app's own imports and works out what satisfies them (ADR-0087), so a
# composition is DERIVED rather than written. These are named entry points for
# that, one per app, and the names are what the docs and the README already use.
# --------------------------------------------------------------------------

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

# Compose saga-domain (docs/apps/SAGA.md — a durable trip-booking saga) with the durable
# primitives it orchestrates: records + fsm + idempotency + event-bus + ids.
# No auth (anonymous engine). Remaining imports are generic WASI.
compose-saga: build
    @just _derive saga-domain {{saga_composed}}

# Compose pulse-domain (docs/apps/REALTIME.md — a realtime chat room with SSE server-push)
# with records + event-bus + id-generate. No auth. Remaining imports are WASI.
compose-pulse: build
    @just _derive pulse-domain {{pulse_composed}}

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

# Compose booked-domain (docs/apps/BOOKED.md — a Calendly-lite booking service) with the
# composed auth-guard + records + lock-mutex (no double-book) + email-render
# (confirmation) + ical (.ics) + rrule (recurring). Remaining imports are WASI.
compose-booked: compose
    @just _derive booked-domain {{booked_composed}}

# Compose transit-domain (docs/apps/TRANSIT.md — a public-transport ticketing service)
# with auth-guard + records (single-use enforced by record-revision CAS) + qr
# (the scannable ticket). Remaining imports are WASI.
compose-transit: compose
    @just _derive transit-domain {{transit_composed}}

# Compose dashboards-domain (docs/apps/DASHBOARDS.md — personal metric dashboards) with
# auth-guard + records + svg-chart (server-side SVG chart rendering). Remaining
# imports are WASI.
compose-dashboards: compose
    @just _derive dashboards-domain {{dashboards_composed}}

# Compose gate-domain (docs/apps/GATE.md — a durable traffic-shaping gateway) with records
# (the durable per-key state) + shaper (the token-bucket / GCRA math). The three
# patterns — rate limit, throttle, batch — are the Golem durable-worker model
# expressed over records:store revision CAS. Remaining imports are WASI.
compose-gate: compose
    @just _derive gate-domain {{gate_composed}}

# Compose books-domain (docs/apps/BOOKS.md — double-entry bookkeeping) with auth-guard +
# records + ledger (the debits==credits invariant + trial balance) + pdf
# (statements). Remaining imports are WASI.
compose-books: compose
    @just _derive books-domain {{books_composed}}

# Compose stash-domain (docs/apps/STASH.md — a note stash you export as a .zip) with
# auth-guard + records + zip (the archive) + csv (the index inside it). Remaining
# imports are WASI.
compose-stash: compose
    @just _derive stash-domain {{stash_composed}}

# Compose payees-domain (docs/apps/PAYEES.md — a payee book) with auth-guard + records +
# iban (validate the IBAN before storing). Remaining imports are WASI.
compose-payees: compose
    @just _derive payees-domain {{payees_composed}}

# Compose lms-domain (docs/apps/LMS.md — a learning platform) with auth-guard + records +
# quiz (auto-grade + stats) + pdf (certificate) + svg-chart (gradebook chart).
# Remaining imports are WASI.
compose-lms: compose
    @just _derive lms-domain {{lms_composed}}

# Compose buzz-domain (docs/apps/BUZZ.md — a live multiplayer quiz game) with auth-guard +
# records. Remaining imports are WASI (random for the PIN, clocks for timing).
compose-buzz: compose
    @just _derive buzz-domain {{buzz_composed}}

# Compose photosocial-domain (docs/apps/PHOTOSOCIAL.md — social photo sharing with AI critique & RBAC attributes)
compose-photosocial: compose
    @just _derive photosocial-domain {{photosocial_composed}}

# Compose mesh-domain (docs/apps/MESH.md — resilient upstream calls) with records (the
# durable per-key circuit state) + resilience (the breaker state machine and the
# backoff schedule) + proxy-route (the REAL outgoing HTTP hop). Remaining imports
# are WASI: clocks for latency + the backoff sleep, config for the route table.
compose-mesh: compose
    @just _derive mesh-domain {{mesh_composed}}

# Compose passkey-domain (docs/apps/PASSKEY.md — passwordless WebAuthn sign-in) with
# webauthn (the ceremony verification: CBOR/COSE + ES256/RS256 signatures) +
# records (accounts + credentials) + cache (single-use challenges with a TTL) +
# session-store (the session a completed ceremony mints). Remaining imports are
# WASI: random for challenges, clocks, and config for the RP id + origin.
compose-passkey: build
    @just _derive cache components/target/cache.composed.wasm
    @just _derive passkey-domain {{passkey_composed}}

# Compose studio-domain (docs/apps/STUDIO.md — the composition studio) with wit-reflect
# (inspection + wac's own composition engine) + records (surfaces + saved
# canvases) + blob-store (the uploaded component bytes). Remaining imports are
# WASI. Note wit_reflect.wasm is ~1 MB: it carries wasmparser and wac-graph, so
# the studio can compose for real instead of printing instructions.
compose-studio: build
    @just _derive studio-domain {{studio_composed}}

# Compose platform-domain (docs/adr/ — the multi-tenant deployment platform) with
# the composed auth-guard (accounts/sessions/RBAC) + policy-guard (ownership and
# visibility as rules) + records (tenants/deployments/revisions) + blob (staged
# uploads) + quota (per-tenant budgets) + wit-reflect (inspect/plan/compose).
# Remaining imports are WASI. Note it no longer needs outgoing-handler for anything:
# with the applier gone (ADR-0022) the control plane makes no outbound calls at all,
# so it runs with egress denied.
compose-platform: compose
    @just _derive platform-domain {{platform_composed}}

# Compose arena-domain (docs/apps/ARENA.md — multiplayer Connect Four) with records +
# id-generate. Remaining imports are WASI.
compose-arena: build
    @just _derive arena-domain {{arena_composed}}

# Golem-backed variant: same queue, but durable:workflow is satisfied by the
# golem-bridge component (calls a durable Golem worker over wasi:http) instead of
# the in-process backend. The composed wasm now imports wasi:http/outgoing-handler
# (a host interface on the v2 operator). Point it at Golem via CFG_GOLEM_URL.
compose-jobs-golem: build
    @just _derive jobs-domain components/target/jobs_domain.golem.wasm

jobs_reg := env_var_or_default("JOBS_REG", "localhost:30501")

# Compose scribe-domain (docs/apps/SCRIBE.md — a collaborative document editor) with the
# crdt merge component + records + id-generate. Remaining imports are WASI.
compose-scribe: build
    @just _derive scribe-domain {{scribe_composed}}

# Compose pipeline-domain (docs/apps/PIPELINE.md — a reliable event pipeline with
# outbox → dispatch → DLQ → replay, SSE server-push) with outbox + event-bus +
# id-generate. No auth. Remaining imports are WASI (bound at deploy).
compose-pipeline: build
    @just _derive pipeline-domain {{pipeline_composed}}

# Compose flags-domain (docs/apps/FLAGS.md — a live feature-rollout console with SSE
# server-push) with feature-flags + event-bus + id-generate. No auth. Remaining
# imports are WASI (kv + config bound at deploy).
compose-flags: build
    @just _derive flags-domain {{flags_composed}}

# Compose abtest-domain (docs/apps/EXPERIMENT.md — an A/B/n experiment console with SSE)
# with experiment-assign + metrics-collect + event-bus + id-generate. No auth.
# Remaining imports are WASI (kv + config bound at deploy).
compose-abtest: build
    @just _derive abtest-domain {{abtest_composed}}

# Compose search-domain (docs/apps/SEARCH.md — faceted search-as-you-type) with the
# engine + corpus + cache (pre-composed with its kv backing) + metrics +
# pagination + ids. No auth. Remaining imports are WASI (kv + config).
compose-search: build
    @just _derive cache components/target/cache.composed.wasm
    @just _derive search-domain {{search_composed}}

# Compose throttle-domain (docs/apps/RATELIMIT.md — a live throttle wall) with the two
# limiters + event-bus + id-generate. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-ratelimit: build
    @just _derive throttle-domain {{throttle_composed}}

# Compose upload-drop (docs/apps/DROP.md — a presigned direct-upload drop-box) with the
# gate + blob store + signer + records + ids. No auth. Remaining imports are
# WASI (kv + config bound at deploy — see CFG_* below).
compose-drop: build
    @just _derive upload-drop {{drop_composed}}

# Compose csv-report (docs/apps/REPORT.md — batch CSV import/report) with the codec +
# validator + records + pagination. No auth. Remaining imports are WASI
# (kv + config bound at deploy).
compose-report: build
    @just _derive csv-report {{report_composed}}

# Compose mfa-authgate (docs/apps/AUTHGATE.md — TOTP 2FA + challenge-response login) with
# the otp primitive + secrets vault + session store + records. No auth-guard —
# this app IS the second factor. secrets:vault needs a 32-byte base64 master-key
# from config (CFG_MASTER_KEY below).
compose-authgate: build
    @just _derive mfa-authgate {{authgate_composed}}

compose-events: build
    @just _derive events-domain {{events_composed}}

compose-paste: build
    @just _derive paste-bin {{paste_composed}}

# Compose track-domain (docs/apps/TRACK.md — a Linear-lite project tracker) — the biggest
# composition in the repo: the pre-composed auth-guard + records + fsm + search +
# event-bus + notify + webhook-sign + policy + paginate + markdown + the
# pre-composed ai-inference (mock llm) + the baked SPA (track-assets). Five axes
# in one self-contained component. Depends on `compose` (guard), `compose-ai`
# (ai+mock-llm), and the built SPA.
compose-track: build-track-ui compose compose-ai
    @just _derive track-domain {{track_composed}}

# Compose grocery-domain (docs/apps/GROCERY.md) with barcode:read and grocery-assets (embedded React SPA)
compose-grocery: build-grocery-ui
    cd components && cargo build --release --target wasm32-wasip2 -p grocery-assets -p grocery-domain -p barcode-read
    @mkdir -p components/target/grocery-override
    @cp components/target/wasm32-wasip2/release/grocery_assets.wasm components/target/grocery-override/
    @cd reconciler && cargo build --release --quiet --bin comp-plug
    @cp "$(./reconciler/target/release/comp-plug grocery-domain --dir components/target/grocery-override --out components/target/composed)" {{grocery_composed}}
    @echo "composed grocery-domain -> {{grocery_composed}}"

# Compose binder-domain (docs/apps/BINDER.md — a Pokemon card binder) with the three
# capabilities it imports: card:identify (a vision model's answer into typed fields),
# price:history (what a card was worth, carried across gaps) and portfolio:value
# (FIFO cost basis, realised and unrealised gain). Remaining imports are WASI.
#
# Derived from the component's own imports (ADR-0087) rather than a hand-written
# `wac plug` line, so adding a capability to the world is the only edit needed.
compose-binder: build
    @just _derive binder-domain {{binder_composed}}

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

# Compose the dev-portal app: the composed auth-guard (auth:identity) +
# record-store + id-generate + quota + policy-guard + outbox + webhook-sign +
# notify-dispatch. RBAC gates role verbs, policy-guard gates project access;
# key events leave as stripe-signed webhooks on an admin-pumped outbox drain.
compose-portal: compose
    @just _derive dev-portal {{portal_composed}}

# Compose the webhook-relay app: the composed webhook-ingest (HMAC verify +
# replay dedup) + jsonpatch + outbox + webhook-sign + notify-dispatch +
# rate-limiter + audit-log + record-store. Ingest -> transform -> durable
# queue; drain delivers github-signed webhooks with retry + dead letters.
compose-relay: compose-webhook
    @just _derive webhook-relay {{relay_composed}}

# Compose the billing-ledger app: money + record-store + idempotency-guard +
# quota + csv + outbox. Idempotency-key replay cache on the write path,
# integer minor-unit arithmetic, revision-CAS balances, csv statements.
compose-ledger: build
    @just _derive billing-ledger {{ledger_composed}}

# Compose the status-page app: scheduler-timer + record-store + fsm-workflow +
# event-bus + notify-dispatch. Timer-driven probes over outgoing HTTP; state
# transitions fan out on the bus and alert as webhooks.
compose-status: build
    @just _derive status-page {{statuspage_composed}}

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

compose-graphviz: build
    @just _derive graph-viz-domain {{graphviz_composed}}

compose-health-records: compose
	@just _derive health-records-domain {{health_records_composed}}

compose-freight-tracker: compose
	@just _derive freight-tracker-domain {{freight_tracker_composed}}

compose-smart-home: compose
	@just _derive smart-home-domain {{smart_home_composed}}

compose-academic-review: compose
	@just _derive academic-review-domain {{academic_review_composed}}

compose-real-estate-escrow: compose
	@just _derive real-estate-escrow-domain {{real_estate_escrow_composed}}

compose-iot-scanner: compose
	@just _derive iot-scanner {{iot_scanner_composed}}

compose-device-radar: compose-iot-scanner
	@just _derive device-radar-domain {{device_radar_composed}}

compose-desktop-notifier: build
    @just _derive desktop-notifier-domain components/target/desktop-notifier.composed.wasm

compose-clipboard-sync: build
    @just _derive clipboard-sync-domain components/target/clipboard-sync.composed.wasm

compose-pdf-generator: build
    @just _derive pdf-generator-domain components/target/pdf-generator.composed.wasm

compose-local-ai: build
    @just _derive local-ai-domain components/target/local-ai.composed.wasm

compose-docker-manager: build
    @just _derive docker-manager-domain components/target/docker-manager.composed.wasm

compose-video-transcoder: build
    @just _derive video-transcoder-domain components/target/video-transcoder.composed.wasm

compose-lan-scanner: build
    @just _derive lan-scanner-domain components/target/lan-scanner.composed.wasm

compose-mdns-discoverer: build
    @just _derive mdns-discoverer-domain components/target/mdns-discoverer.composed.wasm

compose-fs-watcher: build
    @just _derive fs-watcher-domain components/target/fs-watcher.composed.wasm

compose-vpn-manager: build
    @just _derive vpn-manager-domain components/target/vpn-manager.composed.wasm

compose-image-optimizer: build
    @just _derive image-optimizer-domain components/target/image-optimizer.composed.wasm

compose-cron-scheduler: build
    @just _derive cron-scheduler-domain components/target/cron-scheduler.composed.wasm
