#!/usr/bin/env bash
# Does it actually scale? The unit tests prove the arithmetic; this proves the wiring:
# ingress observes concurrency -> publishes it -> reconciler reads it -> placement changes.
#
# min 1, max 4, target 10 concurrent per replica. Idle, then 40 concurrent, then idle.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT
mkdir -p "$SP/nats"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py bench/autoscale/scaled-app.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
for n in 1 2 3; do
  mkdir -p "$SP/n$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "n$n" \
    --lattice auto --addr "127.0.0.1:384$n" --advertise-addr "127.0.0.1:384$n" \
    --state-dir "$SP/n$n" >"$SP/n$n.log" 2>&1 & PIDS+=($!)
done
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice auto --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8096 \
  --nats-url nats://127.0.0.1:4232 --lattice auto --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)

# Sample the replica total throughout, the same way the failover bench does.
# `sleep 2` matters: without it the loop burns every iteration in the first 30
# seconds and the whole trace is the settling period, which reads as "it never
# scaled" when it simply was not watching.
( for i in $(seq 1 70); do
    printf "%s %s\n" "$(date +%s)" \
      "$(nats --server 127.0.0.1:4232 kv ls comp-inventory 2>/dev/null | while read -r k; do
           nats --server 127.0.0.1:4232 kv get comp-inventory "$k" --raw 2>/dev/null; echo; done \
         | python3 -c "import sys,json;print(sum(i.get('count',0) for l in sys.stdin if l.strip() for i in json.loads(l).get('instances',[])))" 2>/dev/null)"
    sleep 2
  done ) >"$SP/replicas.txt" 2>/dev/null & PIDS+=($!)

echo "  settling at min..."; sleep 25
echo "  40 concurrent for 40s (target is 10 per replica, so it should want 4)..."
oha -z 40s -c 40 --no-tui -m POST -d '{"key":"a","capacity":100000000,"refill":100000000}' \
  -H 'content-type: application/json' -H 'Host: shop.eve.test' \
  http://127.0.0.1:8096/api/ratelimit >"$SP/oha.txt" 2>&1
echo "  idle again for 45s (scale-down needs two settled passes)..."; sleep 45
echo
python3 bench/autoscale/trace.py "$SP"
