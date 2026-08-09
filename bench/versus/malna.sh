#!/usr/bin/env bash
# The Pi, both platforms, same component, same generator, same box.
#
# The Mac comparison (ADR-0039) had one uncomfortable asymmetry: wasmCloud was
# reached over OrbStack's cluster network and comp over loopback. On malna there is
# no cluster and no VM — both runtimes are ordinary processes listening on
# 0.0.0.0:9191 and :3861, reached over the same LAN from the same generator. So this
# is the cleaner of the two comparisons, on the slower of the two machines.
#
# wasmCloud runs the 2.5.2 image under podman (matching the control plane exactly);
# comp runs the aarch64 comp-host already deployed there.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
PI=192.168.100.14
KEY="$HOME/.ssh/markkovari_picur_ssh"
MAC=192.168.100.8
DURATION=${DURATION:-20s}
CONNS=${CONNS:-30}
BODY='{"key":"k","capacity":100000000,"refill":100000000}'
PIDS=()
cleanup() {
  ssh -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
    "bash -lc 'pkill -f comp-lattice/comp-host; rm -rf ~/comp-lattice/v1 ~/comp-lattice/v1.log'" >/dev/null 2>&1
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  sleep 1
}
trap cleanup EXIT

# comp's control plane stays on the Mac; only the NODE runs on the Pi, which is the
# same shape as the wasmCloud side (control plane on the Mac, host on the Pi).
mkdir -p "$SP/nats"
nats-server -js -sd "$SP/nats" -a 0.0.0.0 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 bench/adversarial/stub-control-plane.py bench/versus/one-replica.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
ssh -f -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
  "bash -lc 'mkdir -p ~/comp-lattice/v1; exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-v --lattice versus --addr 0.0.0.0:3861 --advertise-addr $PI:3861 --state-dir ~/comp-lattice/v1 > ~/comp-lattice/v1.log 2>&1'"
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice versus --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
echo "  waiting for comp to place onto the Pi..."
sleep 28

echo "=== reachability ==="
for spec in "wasmcloud http://$PI:9191 gate.bench.svc.cluster.local" "comp http://$PI:3861 shop.eve.test"; do
  set -- $spec
  code=$(curl -s -m 10 -o /dev/null -w "%{http_code}" -X POST "$2/api/ratelimit" \
    -H 'content-type: application/json' -H "Host: $3" -d "$BODY")
  echo "    $1: HTTP $code"
done

run() { # label url host
  oha -z "$DURATION" -c "$CONNS" --no-tui --output-format json -m POST -d "$BODY" \
    -H 'content-type: application/json' -H "Host: $3" \
    "$2/api/ratelimit" >"$SP/oha-$1.json" 2>"$SP/oha-$1.err"
  python3 bench/stress/summarise.py "$SP/oha-$1.json" "$1"
}
echo
echo "=== ${DURATION} x ${CONNS} connections, both runtimes on the Pi, load from this Mac ==="
run wasmcloud-pi "http://$PI:9191" gate.bench.svc.cluster.local
run comp-pi      "http://$PI:3861" shop.eve.test

echo
echo "=== resident memory on the Pi while serving ==="
ssh -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
  "bash -lc 'ps -o rss=,comm= -C wash,comp-host 2>/dev/null | sort -rn | head -4 | awk \"{printf \\\"    %-12s %6.0f MiB\\n\\\", \\\$2, \\\$1/1024}\"'"
