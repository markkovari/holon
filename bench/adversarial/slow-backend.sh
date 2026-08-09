#!/usr/bin/env bash
# Round robin's weak case: a backend that is UP but SLOW.
#
# No need to simulate one. A Raspberry Pi 5 is roughly half a MacBook on wasm
# paths, so a fleet of 2 Mac nodes + 2 Pi nodes IS the heterogeneous case, and an
# even split of requests makes the Pi the bottleneck for everyone.
#
# Same fleet, same load, both algorithms.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
MAC=${MAC:-192.168.100.8}
PI=${PI:-192.168.100.14}
KEY="$HOME/.ssh/markkovari_picur_ssh"
rsh() { ssh -n -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=10 markkovari@$PI "bash -lc '$1'"; }
PIDS=()
cleanup() {
  rsh 'pkill -f comp-lattice/comp-host' >/dev/null 2>&1
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  sleep 1
}
trap cleanup EXIT

mkdir -p "$SP/nats"
nats-server -js -sd "$SP/nats" -a 0.0.0.0 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-stub --spec fixtures/five-replicas.yaml \
  --artifact gate=components/target/gate_domain.composed.wasm --port 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2

for n in 1 2; do
  mkdir -p "$SP/mac$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "mac-$n" \
    --lattice slow --addr "127.0.0.1:381$n" --advertise-addr "127.0.0.1:381$n" \
    --state-dir "$SP/mac$n" >"$SP/mac$n.log" 2>&1 & PIDS+=($!)
done
for n in 1 2; do
  ssh -f -n -i "$KEY" -o IdentitiesOnly=yes markkovari@$PI \
    "bash -lc 'exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-$n --lattice slow --addr 0.0.0.0:381$n --advertise-addr $PI:381$n --state-dir ~/comp-lattice/n$n > ~/comp-lattice/n$n.log 2>&1'"
done
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice slow --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
sleep 22

# How much slower IS the Pi? Measure it directly, so the comparison below has a
# reason rather than an assumption.
echo "=== per-node latency, measured not assumed ==="
for target in "mac-1 127.0.0.1:3811" "pi-1 $PI:3811"; do
  set -- $target
  oha -z 5s -c 8 --no-tui -m POST -d '{"key":"probe"}' \
    -H 'content-type: application/json' -H 'Host: shop.eve.test' \
    "http://$2/api/ratelimit" 2>/dev/null \
    | awk -v n="$1" '/Requests\/sec/{r=$2} /^  50.00%/{p=$3} END{printf "  %-6s %8.0f rps   p50 %s\n", n, r, p}'
done

for mode in round-robin least-outstanding; do
  echo
  echo "=== balance: $mode ==="
  ./reconciler/target/release/comp-ingress --addr 127.0.0.1:8091 \
    --nats-url nats://127.0.0.1:4232 --lattice slow --refresh-secs 2 \
    --balance "$mode" >"$SP/ing-$mode.log" 2>&1 &
  ING=$!
  sleep 4
  oha -z 20s -c 60 --no-tui -m POST -d '{"key":"load"}' \
    -H 'content-type: application/json' -H 'Host: shop.eve.test' \
    http://127.0.0.1:8091/api/ratelimit >"$SP/oha-$mode.txt" 2>&1
  grep -E "Requests/sec|Success rate|^  50.00%|^  95.00%|^  99.00%" "$SP/oha-$mode.txt" | sed 's/^/  /'
  kill $ING 2>/dev/null; sleep 1
done
