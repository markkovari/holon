#!/usr/bin/env bash
# Cross-node invocation: ONE app whose graph is split across two nodes.
#
#   gate         (role=web)  imports records:store/store and shaper:limit/limiter
#   record-store (role=data) exports records:store/store   <- on the OTHER node
#   shaper                   exports shaper:limit/limiter  <- co-located with gate
#
# So one link is an in-process call and the other must cross the wire. If gate can
# serve a request, a component called a component on another node.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT
mkdir -p "$SP/nats" "$SP/web" "$SP/data"

nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
R=components/target/wasm32-wasip2/release
python3 $HERE/stub-control-plane.py $HERE/split-graph.json \
  "{\"gate\":\"$R/gate_domain.wasm\",\"record-store\":\"$R/record_store.wasm\",\"shaper\":\"$R/shaper.wasm\"}" \
  8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2

./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node web-1 --lattice split \
  --addr 127.0.0.1:3901 --advertise-addr 127.0.0.1:3901 --state-dir "$SP/web" \
  --label role=web >"$SP/web.log" 2>&1 & PIDS+=($!)
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node data-1 --lattice split \
  --addr 127.0.0.1:3902 --advertise-addr 127.0.0.1:3902 --state-dir "$SP/data" \
  --label role=data >"$SP/data.log" 2>&1 & PIDS+=($!)
sleep 3
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 --secret test-secret \
  --nats-url nats://127.0.0.1:4232 --lattice split --interval 3 >"$SP/rec.log" 2>&1 & PIDS+=($!)
sleep 22

echo "=== placement: the graph must be SPLIT, not co-located ==="
echo "  web-1:"; grep -E "started|serves|bound|links" "$SP/web.log" | sed 's/^/    /'
echo "  data-1:"; grep -E "started|serves|bound|links" "$SP/data.log" | sed 's/^/    /'
echo
echo "=== the call: gate is on web-1, record-store is on data-1 ==="
python3 - <<'PY'
import json, urllib.request
def hit(key):
    r = urllib.request.Request("http://127.0.0.1:3901/api/ratelimit",
        data=json.dumps({"key": key}).encode(),
        headers={"content-type":"application/json","Host":"split.alice.test"})
    try:
        with urllib.request.urlopen(r, timeout=20) as f:
            return json.loads(f.read())
    except Exception as e:
        return {"error": str(e)[:160]}
first = hit("cross-node-1")
print("   ", first)
if "remaining" in first:
    second = hit("cross-node-1")
    print("   ", second)
    ok = second["remaining"] < first["remaining"]
    print(f"\n  RESULT: {'CROSS-NODE CALL WORKS — state advanced via a component on the other node' if ok else 'served, but state did not advance'}")
else:
    print("\n  RESULT: the call did not complete")
PY
echo
echo "=== the reconciler's view ==="; tail -3 "$SP/rec.log" | sed 's/^/  /'
