#!/usr/bin/env bash
# Cross-machine failover, measured THROUGH the failure rather than after it.
#
# `bench/adversarial/five-nodes.sh` already kills the Pi nodes — but it sleeps 20s
# first and then sends 60 requests, which asks "did it recover" and skips "what did
# it cost". The interesting window is the one that script sleeps through: between a
# machine dying and the ingress noticing, requests are being routed at a corpse.
#
# So: continuous load through one address while nodes are SIGKILLed underneath it,
# bucketed per second. Two directions, because they are not the same claim —
#
#   A. kill a node on THIS machine, and the replacement must land on the Pi
#   B. kill the whole Pi, and the replacements must come home
#
# Only A proves recovery is not machine-local; only B proves surviving a machine.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
MAC=${MAC:-192.168.100.8}
PI=${PI:-192.168.100.14}
KEY="$HOME/.ssh/markkovari_picur_ssh"
rsh() { ssh -n -i "$KEY" -o IdentitiesOnly=yes -o ConnectTimeout=10 "markkovari@$PI" "bash -lc '$1'"; }
PIDS=()
cleanup() {
  rsh 'pkill -f comp-lattice/comp-host; rm -rf ~/comp-lattice/f1 ~/comp-lattice/f2 ~/comp-lattice/f*.log' >/dev/null 2>&1
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
    --lattice fail --addr "127.0.0.1:381$n" --advertise-addr "127.0.0.1:381$n" \
    --state-dir "$SP/mac$n" --label box=mac >"$SP/mac$n.log" 2>&1 & PIDS+=($!)
done
for n in 1 2; do
  ssh -f -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
    "bash -lc 'exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-$n --lattice fail --addr 0.0.0.0:381$n --advertise-addr $PI:381$n --state-dir ~/comp-lattice/f$n --label box=pi > ~/comp-lattice/f$n.log 2>&1'"
done
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice fail --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8090 \
  --nats-url nats://127.0.0.1:4232 --lattice fail --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)

echo "  settling (5 replicas over 4 nodes, two machines)..."
sleep 26
echo "=== before ==="
./reconciler/target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice fail

# The kills, on a timer, so the load generator below runs uninterrupted across them.
( sleep 15; echo "$(date +%s) kill mac-2" >>"$SP/events.txt"
  pkill -9 -f "node mac-2 --lattice fail"
  sleep 45; echo "$(date +%s) kill the Pi" >>"$SP/events.txt"
  ssh -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" "bash -lc 'pkill -9 -f comp-lattice/comp-host'" ) &
PIDS+=($!)

# Sample the fleet's replica total while the load runs. Zero dropped requests is
# only half of failover; the other half is how long the fleet stays under-replicated.
#
# `kv get --raw` writes NO trailing newline, so several nodes concatenate into one
# invalid JSON line — which silently produced an empty sample for every second the
# fleet had more than one node alive, i.e. exactly the interval being measured. The
# `echo` is the fix. Resolution is one sample per ~5s: five CLI startups per pass.
( for i in $(seq 1 110); do
    printf "%s %s\n" "$(date +%s)" \
      "$(nats --server 127.0.0.1:4232 kv ls comp-inventory 2>/dev/null | while read -r k; do
           nats --server 127.0.0.1:4232 kv get comp-inventory "$k" --raw 2>/dev/null; echo; done \
         | ./reconciler/target/release/comp-bench replicas 2>/dev/null)"
    sleep 1
  done ) >"$SP/replicas.txt" 2>/dev/null & PIDS+=($!)

echo
echo "=== load across both failures: 100s, bucketed per second ==="
./reconciler/target/release/comp-bench load "http://127.0.0.1:8090/api/ratelimit" --host shop.eve.test --secs 100 --rate 2600 --events "$SP/events.txt"

echo
echo "=== after ==="
./reconciler/target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice fail
echo
echo "=== how long the fleet ran under-replicated ==="
./reconciler/target/release/comp-bench converge "$SP/replicas.txt" --events "$SP/events.txt" --want 5
echo
echo "=== what the reconciler did (re-placements, in order) ==="
grep -h "started" "$SP"/mac*.log | sed 's/ (sha.*//;s/comp-host: //' | cat -n | sed 's/^/  /'
