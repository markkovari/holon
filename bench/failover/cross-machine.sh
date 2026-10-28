#!/usr/bin/env bash
# Cross-machine failover measured THROUGH the failure. Killing a local node proves recovery
# is not machine-local; killing the whole Pi proves surviving a machine.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
MAC=${MAC:-192.168.1.10}
PI=${PI:-192.168.1.11}
KEY=${KEY:-"$HOME/.ssh/id_ed25519"}
# Without the Pi this would report a clean failover of a fleet that never spanned machines.
. bench/preflight.sh
need_cmd nats-server ssh nats
need_local_addr "$MAC"
need_remote "markkovari@$PI" "$KEY" "the Pi ($PI)"
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
./target/release/comp-stub --spec fixtures/five-replicas.yaml \
  --artifact gate=components/target/gate_domain.composed.wasm --port 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2

for n in 1 2; do
  mkdir -p "$SP/mac$n"
  ./target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "mac-$n" \
    --lattice fail --addr "127.0.0.1:381$n" --advertise-addr "127.0.0.1:381$n" \
    --state-dir "$SP/mac$n" --label box=mac >"$SP/mac$n.log" 2>&1 & PIDS+=($!)
done
for n in 1 2; do
  ssh -f -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" \
    "bash -lc 'exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-$n --lattice fail --addr 0.0.0.0:381$n --advertise-addr $PI:381$n --state-dir ~/comp-lattice/f$n --label box=pi > ~/comp-lattice/f$n.log 2>&1'"
done
sleep 3
./target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice fail --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./target/release/comp-ingress --addr 127.0.0.1:8090 \
  --nats-url nats://127.0.0.1:4232 --lattice fail --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)

echo "  settling (5 replicas over 4 nodes, two machines)..."
sleep 26
echo "=== before ==="
./target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice fail

( sleep 15; echo "$(date +%s) kill mac-2" >>"$SP/events.txt"
  pkill -9 -f "node mac-2 --lattice fail"
  sleep 45; echo "$(date +%s) kill the Pi" >>"$SP/events.txt"
  ssh -n -i "$KEY" -o IdentitiesOnly=yes "markkovari@$PI" "bash -lc 'pkill -9 -f comp-lattice/comp-host'" ) &
PIDS+=($!)

# `kv get --raw` writes no trailing newline; the `echo` keeps one JSON document per line.
( for i in $(seq 1 110); do
    printf "%s %s\n" "$(date +%s)" \
      "$(nats --server 127.0.0.1:4232 kv ls comp-inventory 2>/dev/null | while read -r k; do
           nats --server 127.0.0.1:4232 kv get comp-inventory "$k" --raw 2>/dev/null; echo; done \
         | ./target/release/comp-bench replicas 2>/dev/null)"
    sleep 1
  done ) >"$SP/replicas.txt" 2>/dev/null & PIDS+=($!)

echo
echo "=== load across both failures: 100s, bucketed per second ==="
./target/release/comp-bench load "http://127.0.0.1:8090/api/ratelimit" --host shop.eve.test --secs 100 --rate 2600 --events "$SP/events.txt"

echo
echo "=== after ==="
./target/release/comp-bench inventory --nats-url nats://127.0.0.1:4232 --lattice fail
echo
echo "=== how long the fleet ran under-replicated ==="
./target/release/comp-bench converge "$SP/replicas.txt" --events "$SP/events.txt" --want 5
echo
echo "=== what the reconciler did (re-placements, in order) ==="
grep -h "started" "$SP"/mac*.log | sed 's/ (sha.*//;s/comp-host: //' | cat -n | sed 's/^/  /'
