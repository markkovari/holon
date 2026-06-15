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
