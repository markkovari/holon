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
      -o {{vet_composed}}
    @echo "composed vet-domain (+ auth-guard + records + validate + search) -> {{vet_composed}}"

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
