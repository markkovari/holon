#!/usr/bin/env bash
# What a cold start actually costs — the number that decides whether scale-to-zero
# is worth an activation path or whether a very small `min` is the better trade.
#
# Three distinct costs hide inside "cold start" and only one is worth optimising:
#
#   fetch    pull the artifact from the object store (or hit the node's cache)
#   compile  wasmtime turning wasm into machine code
#   link     build the linker, bind remote imports, instantiate_pre
#
# So the host reports them per phase on its own start line, rather than being timed
# from the far side of a NATS round trip where the CLI's own startup (~100ms) would
# swamp the thing being measured.
#
# The reconciler is KILLED before the measurement: it re-places a missing instance
# within one interval, which would race every stop/start pair here.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)/comp"
SP=${SP:-$(mktemp -d)}
HERE=bench/adversarial
N=${N:-10}
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 1' EXIT

mkdir -p "$SP/nats" "$SP/n1"
nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4232 >"$SP/nats.log" 2>&1 & PIDS+=($!)
python3 $HERE/stub-control-plane.py $HERE/five-replicas.json \
  '{"gate":"components/target/gate_domain.composed.wasm"}' 8099 >"$SP/plat.log" 2>&1 & PIDS+=($!)
sleep 2
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4232 --node n1 \
  --lattice cold --addr 127.0.0.1:3801 --advertise-addr 127.0.0.1:3801 \
  --state-dir "$SP/n1" >"$SP/n1.log" 2>&1 & PIDS+=($!)
sleep 2
./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8099 \
  --secret test-secret --nats-url nats://127.0.0.1:4232 --lattice cold --interval 3 \
  >"$SP/rec.log" 2>&1 & REC=$!
PIDS+=($REC)

echo "  waiting for the first placement (this one pays the object-store pull)..."
for _ in $(seq 1 40); do grep -q "started" "$SP/n1.log" && break; sleep 1; done
kill $REC 2>/dev/null; sleep 1
echo "  reconciler stopped; driving start/stop by hand from here"
echo
python3 bench/coldstart/cycle.py "$SP" "$N"

echo
echo "=== every start this node logged ==="
grep "started .* in " "$SP/n1.log" | sed 's/comp-host: /  /'
echo
python3 bench/coldstart/summarise.py "$SP/n1.log"

# The branch that must never brick a node: a .cwasm written by a different wasmtime
# build, or a truncated write. It is machine code loaded with `deserialize_file`,
# which trusts its input, so the failure has to be caught and the file dropped
# rather than propagated.
echo
echo "=== a corrupt cache must fall back to compiling, not fail the start ==="
python3 bench/coldstart/corrupt.py "$SP"
