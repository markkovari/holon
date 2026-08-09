#!/usr/bin/env bash
# Does shedding make the app grow?
#
# ADR-0041 gave the ingress a bound and ADR-0038 scales on observed concurrency —
# and left alone the two FIGHT: a shed request never becomes in-flight, so the
# reconciler sees a calm app while the ingress refuses traffic at the door.
#
# Four nodes, one app, min 1 / max 4, and a deliberately low --max-inflight so load
# hits the bound rather than the fleet's actual capacity. Replicas should climb.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
BOUND=${BOUND:-8}
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

mkdir -p "$SP/nats"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py bench/shedscale/app.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
for n in 1 2 3 4; do
  mkdir -p "$SP/n$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "n$n" \
    --lattice shed --addr "127.0.0.1:389$n" --advertise-addr "127.0.0.1:389$n" \
    --state-dir "$SP/n$n" >"$SP/n$n.log" 2>&1 & PIDS+=($!)
done
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice shed --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8093 \
  --nats-url nats://127.0.0.1:4232 --lattice shed --refresh-secs 2 \
  --max-inflight "$BOUND" >"$SP/ingress.log" 2>&1 & PIDS+=($!)

echo "  settling at min (bound is $BOUND in flight per node)..."
sleep 24
python3 bench/shedscale/watch.py "$SP"
