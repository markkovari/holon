#!/usr/bin/env bash
# comp vs wasmCloud 2.x, same component, same machine, same load generator.
#
# Every previous "how much slower are we" answer in this repo came from comparing
# numbers taken from different components on different machines through different
# paths. This runs ONE component — gate-domain, the same .wasm both sides — on both
# platforms on this Mac, and measures four lanes so the platform cost is separable
# from the proxy in front of it:
#
#   wasmcloud-gw    load -> runtime-gateway (ClusterIP) -> wash host -> component
#   wasmcloud-host  load -> wash host :9191 (ClusterIP)  -> component
#   comp-ingress    load -> comp-ingress    (loopback)   -> comp-host  -> component
#   comp-host       load -> comp-host       (loopback)   -> component
#
# Honest asymmetry, stated rather than hidden: wasmCloud is reached over OrbStack's
# cluster network and comp over loopback. That favours comp by some amount this
# script cannot separate, which is why the host-direct lanes are reported too — they
# are the closest thing to a like-for-like pair.
#
# The body sets capacity/refill high so both sides exercise the ALLOW path (a token
# bucket at its default limit answers 429 from a cheap branch that touches no
# storage — ADR-0036).
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
DURATION=${DURATION:-20s}
CONNS=${CONNS:-50}
BODY='{"key":"k","capacity":100000000,"refill":100000000}'
WC_HOSTHDR=${WC_HOSTHDR:-gate.bench.svc.cluster.local}
PIDS=()
trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

GW=$(kubectl -n bench get svc runtime-gateway -o jsonpath='{.spec.clusterIP}' 2>/dev/null)
WHOST=$(kubectl -n bench get svc hostgroup-default -o jsonpath='{.spec.clusterIP}' 2>/dev/null)
if [ -z "$GW" ] || [ -z "$WHOST" ]; then
  echo "wasmCloud stack not found in ns bench" >&2; exit 1
fi

# --- comp side: one node, one ingress, the same component -------------------
mkdir -p "$SP/nats" "$SP/n1"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-stub --spec fixtures/one-replica.yaml \
  --artifact gate=components/target/gate_domain.composed.wasm --port 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node n1 \
  --lattice versus --addr 127.0.0.1:3861 --advertise-addr 127.0.0.1:3861 \
  --state-dir "$SP/n1" >"$SP/n1.log" 2>&1 & PIDS+=($!)
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice versus --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-ingress --addr 127.0.0.1:8098 \
  --nats-url nats://127.0.0.1:4232 --lattice versus --refresh-secs 2 >"$SP/ingress.log" 2>&1 & PIDS+=($!)
echo "  waiting for comp to place the component..."
sleep 26

probe() { # url host label
  curl -s -m 10 -X POST "$1/api/ratelimit" -H 'content-type: application/json' \
    -H "Host: $2" -d "$BODY" -o /dev/null -w "    %{http_code}" 2>/dev/null
  echo "  $3"
}
echo "=== reachability before measuring anything ==="
probe "http://$GW" "$WC_HOSTHDR" "wasmcloud via gateway"
probe "http://$WHOST:9191" "$WC_HOSTHDR" "wasmcloud direct to host"
probe "http://127.0.0.1:8098" "shop.eve.test" "comp via ingress"
probe "http://127.0.0.1:3861" "shop.eve.test" "comp direct to host"

run() { # label url host
  oha -z "$DURATION" -c "$CONNS" --no-tui --output-format json -m POST -d "$BODY" \
    -H 'content-type: application/json' -H "Host: $3" \
    "$2/api/ratelimit" >"$SP/oha-$1.json" 2>"$SP/oha-$1.err"
  ./reconciler/target/release/comp-bench summarise "$SP/oha-$1.json" "$1"
}
echo
echo "=== ${DURATION} x ${CONNS} connections per lane ==="
run wasmcloud-gw   "http://$GW"            "$WC_HOSTHDR"
run wasmcloud-host "http://$WHOST:9191"    "$WC_HOSTHDR"
run comp-ingress   "http://127.0.0.1:8098" "shop.eve.test"
run comp-host      "http://127.0.0.1:3861" "shop.eve.test"

echo
echo "=== memory held by each runtime while serving ==="
kubectl -n bench top pod --no-headers 2>/dev/null | awk '{printf "    %-36s %s\n", $1, $3}' || echo "    (metrics-server unavailable)"
pid=$(pgrep -f "node n1 --lattice versus" | head -1)
[ -n "$pid" ] && ps -o rss= -p "$pid" | awk '{printf "    %-36s %.0fMi\n", "comp-host (n1)", $1/1024}'
