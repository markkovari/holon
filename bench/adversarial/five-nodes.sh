#!/usr/bin/env bash
# Five nodes across two machines, one app, one address in front of it.
#
#   3 nodes on this box, 2 on the Pi, one comp-ingress, one deployment with
#   replicas: 5. Every request goes to the ingress and comes back saying which node
#   answered — which is the only way to see the balance from outside.
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
python3 $HERE/stub-control-plane.py $HERE/five-replicas.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2

echo "=== 3 nodes here ==="
for n in 1 2 3; do
  mkdir -p "$SP/mac$n"
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "mac-$n" \
    --lattice five --addr "127.0.0.1:370$n" --advertise-addr "127.0.0.1:370$n" \
    --state-dir "$SP/mac$n" --label box=mac >"$SP/mac$n.log" 2>&1 & PIDS+=($!)
done
echo "=== 2 nodes on the Pi ==="
for n in 1 2; do
  ssh -f -n -i "$KEY" -o IdentitiesOnly=yes markkovari@$PI \
    "bash -lc 'exec ~/comp-lattice/comp-host --lattice-nats nats://$MAC:4232 --node pi-$n --lattice five --addr 0.0.0.0:370$n --advertise-addr $PI:370$n --state-dir ~/comp-lattice/n$n --label box=pi > ~/comp-lattice/n$n.log 2>&1'"
done
sleep 3

./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice five --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
# TWO ingresses. It holds no state beyond a cache of inventory, so "several can
# run" is either true or it is not — worth checking rather than asserting.
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8090 \
  --nats-url nats://127.0.0.1:4232 --lattice five --refresh-secs 2 \
  >"$SP/ingress.log" 2>&1 &
INGRESS_A=$!
PIDS+=($INGRESS_A)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8095 \
  --nats-url nats://127.0.0.1:4232 --lattice five --refresh-secs 2 \
  >"$SP/ingress-b.log" 2>&1 & PIDS+=($!)

echo "  settling..."
sleep 24
echo
echo "=== what the ingress sees ==="
tail -2 "$SP/ingress.log" | sed 's/^/  /'
echo
echo "=== 200 requests, all to ONE address, counted by which node answered ==="
python3 - <<PY
import json, urllib.request, collections
seen = collections.Counter()
fail = 0
for i in range(200):
    r = urllib.request.Request("http://127.0.0.1:8090/api/ratelimit",
        data=json.dumps({"key": f"k{i}"}).encode(),
        headers={"content-type":"application/json","Host":"shop.eve.test"})
    try:
        with urllib.request.urlopen(r, timeout=15) as resp:
            seen[resp.headers.get("x-comp-node","?")] += 1
    except Exception:
        fail += 1
for node, n in sorted(seen.items()):
    print(f"    {node:10} {n:4}  {'#' * (n // 3)}")
print(f"    {'failed':10} {fail:4}")
print(f"\\n  {len(seen)} distinct node(s) served; spread {min(seen.values())}-{max(seen.values())}" if seen else "  nothing served")
PY
echo
echo "=== HA: two ingresses, then kill one ==="
python3 $HERE/ha-check.py
kill $INGRESS_A 2>/dev/null; sleep 2
python3 $HERE/ha-check.py --only 8095 --label "ingress A killed; B"

echo
echo "=== kill both Pi nodes, then 60 more requests ==="
rsh 'pkill -f comp-lattice/comp-host' >/dev/null 2>&1
sleep 20
python3 - <<PY
import json, urllib.request, collections
seen = collections.Counter(); fail = 0
for i in range(60):
    r = urllib.request.Request("http://127.0.0.1:8095/api/ratelimit",
        data=json.dumps({"key": f"post{i}"}).encode(),
        headers={"content-type":"application/json","Host":"shop.eve.test"})
    try:
        with urllib.request.urlopen(r, timeout=15) as resp:
            seen[resp.headers.get("x-comp-node","?")] += 1
    except Exception:
        fail += 1
for node, n in sorted(seen.items()):
    print(f"    {node:10} {n:4}")
print(f"    {'failed':10} {fail:4}")
print(f"\\n  {'the Pi nodes are gone and traffic kept flowing' if fail == 0 and not any(k.startswith('pi') for k in seen) else 'CHECK: ' + str(dict(seen))}")
PY
