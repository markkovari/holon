#!/usr/bin/env bash
# Run the whole eshop on native hosts: 5 services + gateway, one shared NATS
# JetStream KV (the cross-service backbone: sessions, records, event bus).
# Ctrl-C stops everything. Prereq: `just compose-eshop` + NATS on :4222
# (e.g. docker run -d --name eshop-nats -p 4222:4222 nats:2.10 -js).
set -euo pipefail
cd "$(dirname "$0")/../.."

nc -z 127.0.0.1 4222 || { echo "NATS not reachable on :4222"; exit 1; }
cargo build --release --bin vet-host --manifest-path host/Cargo.toml

H=host/target/release/vet-host
C=components/target
PIDS=()
run() { "$@" & PIDS+=($!); }
trap 'kill "${PIDS[@]}" 2>/dev/null' EXIT

VET_TENANT=eshop run $H --component $C/eshop_identity.composed.wasm --addr 127.0.0.1:3105 --kv nats
VET_TENANT=eshop run $H --component $C/eshop_catalog.composed.wasm  --addr 127.0.0.1:3101 --kv nats
VET_TENANT=eshop run $H --component $C/eshop_basket.composed.wasm   --addr 127.0.0.1:3102 --kv nats
VET_TENANT=eshop CFG_GRACE_PERIOD_SECS=${GRACE:-15} \
  run $H --component $C/eshop_ordering.composed.wasm --addr 127.0.0.1:3103 --kv nats
VET_TENANT=eshop CFG_PAYMENT_SUCCEEDS=${PAYMENT_SUCCEEDS:-true} \
  run $H --component $C/eshop_payment.composed.wasm --addr 127.0.0.1:3104 --kv nats
# gateway = SPA + proxy:route; the route table is deploy-time config.
CFG_ROUTES="/api/identity=http://127.0.0.1:3105/,/api/catalog=http://127.0.0.1:3101,/api/basket=http://127.0.0.1:3102,/api/orders=http://127.0.0.1:3103,/pump/ordering=http://127.0.0.1:3103/internal/pump/,/pump/catalog=http://127.0.0.1:3101/internal/pump/,/pump/payment=http://127.0.0.1:3104/internal/pump/,/pump/basket=http://127.0.0.1:3102/internal/pump/" \
  run $H --component $C/eshop_gateway.composed.wasm --addr 127.0.0.1:3100

echo
echo "eshop up — storefront http://127.0.0.1:3100 (the open page pumps the choreography itself)"
echo "smoke:      GATEWAY=http://127.0.0.1:3100 examples/eshop/smoke.sh"
wait
