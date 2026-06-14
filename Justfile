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
guard_composed := "components/target/auth_guard.composed.wasm"

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

# Compose the rate-limiter into auth-guard with wac, satisfying auth-guard's
# `ratelimit:guard/limiter` import. Output is a single self-contained component.
compose: build
    wac plug {{guard_wasm}} --plug {{ratelimit_wasm}} -o {{guard_composed}}
    @echo "composed auth-guard (+ rate-limiter) -> {{guard_composed}}"

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

# Deploy on wasmCloud 2.x via the Kubernetes operator (needs kubectl + operator).
# Push components to OCI first (see README), and replace REPLACE_ME in workload.yaml.
deploy-k8s:
    kubectl apply -f infra/k8s/host.yaml
    kubectl apply -f infra/k8s/workload.yaml

# Full local check: vendor (if needed), validate WIT, build, validate components.
check: wit-check validate
    @echo "OK — contract resolves and both components build clean"
