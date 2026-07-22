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

# Run the composed status-page under the native host.
host-status: compose-status
    cd host && cargo run --release --bin vet-host -- --component ../{{statuspage_composed}} --addr 127.0.0.1:3012

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
