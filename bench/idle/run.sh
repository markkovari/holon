#!/usr/bin/env bash
# What does an idle app actually cost a node?
#
# All apps use the SAME component digest, which is the marketplace case: many tenants
# deploying one popular component. If the host held one compiled module per digest the
# marginal cost would be a route entry; if it holds one per instance it is a copy of
# the machine code each time.
set -uo pipefail
cd /Users/markkovari/DEV/markkovari/experiments/comp
SP=/private/tmp/claude-501/-Users-markkovari-DEV-markkovari-experiments/0bbb61b5-8beb-4d54-9385-d6fa541d2164/scratchpad/idle
rm -rf "$SP"; mkdir -p "$SP/nats" "$SP/n1" "$SP/specs"
PIDS=(); trap 'for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done' EXIT

N=${N:-16}
for i in $(seq 1 "$N"); do
  cat > "$SP/specs/app$i.yaml" <<EOF
version: comp/v1
app: app$i
tenant: t$i
strategy: fused
components:
  - id: gate
ingress:
  host: app$i.idle.test
  component: gate
EOF
done

nats-server -js -sd "$SP/nats" -a 127.0.0.1 -p 4272 >"$SP/nats.log" 2>&1 & PIDS+=($!)
./reconciler/target/release/comp-stub --spec "$SP/specs" \
  --artifact gate=components/target/gate_domain.composed.wasm --port 8409 >"$SP/stub.log" 2>&1 & PIDS+=($!)
sleep 2
./host/target/release/comp-host --lattice-nats nats://127.0.0.1:4272 --node n1 --lattice idle \
  --addr 127.0.0.1:3991 --advertise-addr 127.0.0.1:3991 --state-dir "$SP/n1" >"$SP/n1.log" 2>&1 & PIDS+=($!)
sleep 2
HOSTPID=$(pgrep -f "node n1 --lattice idle" | head -1)
rss() { ps -o rss= -p "$HOSTPID" | awk '{printf "%.1f", $1/1024}'; }
echo "  idle host, nothing placed:            $(rss) MiB"

./reconciler/target/release/comp-reconciler --platform-url http://127.0.0.1:8409 \
  --secret test-secret --nats-url nats://127.0.0.1:4272 --lattice idle --interval 3 \
  >"$SP/rec.log" 2>&1 & PIDS+=($!)

placed=0
for _ in $(seq 1 40); do
  placed=$(grep -c "started" "$SP/n1.log" 2>/dev/null || echo 0)
  [ "${placed:-0}" -ge "$N" ] 2>/dev/null && break
  sleep 2
done
sleep 3
echo "  after $placed apps placed (same digest): $(rss) MiB"
echo
echo "  per-app marginal: $(ps -o rss= -p "$HOSTPID" | awk -v n="$placed" '{printf "%.2f MiB", ($1/1024 - 12)/n}')  (against a ~12 MiB empty host)"
echo
echo "  how each module arrived (shared means one copy for many apps):"
grep -o "compile [0-9]* us\|cache-load [0-9]* us\|shared [0-9]* us" "$SP/n1.log" \
  | awk '{print $1}' | sort | uniq -c | sed 's/^/  /'
