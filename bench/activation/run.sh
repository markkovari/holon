#!/usr/bin/env bash
# Scale to zero, then bring it back with a request.
#
# ADR-0038 shipped `min: 0` that PARKED an app: no replica, no route, 503, and
# nothing to wake it. ADR-0040 then made a warm start 0.43ms, which is what makes
# holding a request across an activation reasonable rather than absurd.
#
# The measurement is what a caller experiences: the first request to a cold app.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
BODY='{"key":"a","capacity":100000000,"refill":100000000}'
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

mkdir -p "$SP/nats" "$SP/n1"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py bench/activation/zero-app.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node n1 \
  --lattice zero --addr 127.0.0.1:3871 --advertise-addr 127.0.0.1:3871 \
  --state-dir "$SP/n1" >"$SP/n1.log" 2>&1 & PIDS+=($!)
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice zero --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8092 \
  --nats-url nats://127.0.0.1:4232 --lattice zero --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)

echo "  settling — the app should sit at ZERO replicas..."
sleep 22
python3 bench/activation/probe.py "$SP"
