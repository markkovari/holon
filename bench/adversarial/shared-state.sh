#!/usr/bin/env bash
# One app, two replicas, two nodes. Does the second replica CONTINUE the first
# one's count, or start its own?
#
# With node-local stores it starts its own — silently — which is the bug this
# exists to keep fixed. Run A proves the reconciler now refuses that arrangement;
# run B proves a shared store makes it work.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
PIDS=()
trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

mkdir -p "$SP/n1" "$SP/n2" "$SP/nats"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py $HERE/spread-stateful.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2

KV=${KV:-}   # empty = let the host pick its own default
for n in 1 2; do
  ./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node "n$n" \
    --lattice st --addr "127.0.0.1:350$n" --state-dir "$SP/n$n" \
    ${KV:+--kv "$KV"} --sqlite-path "$SP/n$n/kv.db" \
    >"$SP/n$n.log" 2>&1 & PIDS+=($!)
done
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice st --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
sleep 16

echo "=== kv backend: $KV ==="
grep -h "kv = " "$SP/n1.log" | sed 's/^/  /'
placed=$(cat "$SP/n1.log" "$SP/n2.log" 2>/dev/null | grep -c "started" || true)
echo "  instances placed: $placed"
if [ "$placed" -lt 2 ]; then
  echo "  reconciler refused:"
  grep -h "unschedulable" "$SP/rec.log" | tail -1 | sed 's/^/    /'
  exit 0
fi

python3 - <<'PY'
import json, urllib.request
def hit(port, key):
    r = urllib.request.Request(f"http://127.0.0.1:{port}/api/ratelimit",
        data=json.dumps({"key": key}).encode(),
        headers={"content-type": "application/json", "Host": "shop.eve.test"})
    try: return json.loads(urllib.request.urlopen(r, timeout=8).read())["remaining"]
    except Exception as e: return None
print("  one deployment, two replicas, one rate-limit key:")
seen = []
for _ in range(3):
    v = hit(3501, "customer-1"); seen.append(v)
    print(f"    node1 -> remaining {v}")
v = hit(3502, "customer-1"); seen.append(v)
print(f"    node2 -> remaining {v}")
first, last = seen[0], seen[-1]
if last is None:
    print("\\n  RESULT: node2 did not serve"); raise SystemExit(1)
# node2 must continue the count, not restart it.
ok = last < seen[-2]
print(f"\\n  RESULT: {'SHARED — node2 continued the count' if ok else 'SPLIT-BRAIN — node2 restarted at ' + str(last)}")
raise SystemExit(0 if ok else 1)
PY
