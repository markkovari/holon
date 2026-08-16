#!/usr/bin/env bash
# saga app-path bench (docs/apps/SAGA.md rung 4) — the first bench of a stateful WORKFLOW
# path, not stateless CRUD. Every request goes browser → hyper → wasmtime →
# saga_domain.composed.wasm (saga-domain + fsm + records + idempotency +
# event-bus + ids + timer) → wasi:keyvalue backend.
#
# Usage: bench/saga-bench.sh [memory|nats]
set -euo pipefail
KV="${1:-memory}"
DIR="$(cd "$(dirname "$0")" && pwd)"; ROOT="$(cd "$DIR/.." && pwd)"
ADDR=127.0.0.1:3014; B="http://$ADDR"
BIN="$ROOT/host/target/release/comp-host"; COMP="$ROOT/components/target/saga_domain.composed.wasm"
jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{const o=JSON.parse(d);console.log(eval("o"+process.argv[1]))})' "$1"; }
now() { node -e 'console.log(Date.now())'; }

args=(--component "$COMP" --addr "$ADDR" --kv "$KV")
[ "$KV" = "nats" ] && args+=(--nats-url 127.0.0.1:4222)
VET_TENANT=saga "$BIN" "${args[@]}" >/tmp/saga-bench-host.log 2>&1 &
HPID=$!; trap 'kill $HPID 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do curl -sf "$B/" >/dev/null 2>&1 && break; sleep 0.1; done

SEED=$(curl -s -X POST "$B/trips" -d '{"traveler":"seed"}' | jget '.id')
curl -s -X POST "$B/trips/$SEED/run" >/dev/null

oha_row() { # name method path [body]
  local name="$1" method="$2" path="$3" body="${4:-}"; local h=()
  [ -n "$body" ] && h=(-H 'content-type: application/json' -d "$body")
  local j; j=$(oha --output-format json -z 8s -c 20 -m "$method" ${h[@]+"${h[@]}"} "$B$path")
  printf '%-34s %8.0f  %6.1f / %-6.1f\n' "$name" \
    "$(echo "$j" | jget '.summary.requestsPerSec')" \
    "$(echo "$j" | jget '.latencyPercentiles.p50*1000')" \
    "$(echo "$j" | jget '.latencyPercentiles.p99*1000')"
}

batch() { # label body N  -> full create+run latency
  local label="$1" body="$2" n="$3" t0 t1
  t0=$(now)
  for _ in $(seq 1 "$n"); do
    local id; id=$(curl -s -X POST "$B/trips" -d "$body" | jget '.id')
    curl -s -X POST "$B/trips/$id/run" >/dev/null
  done
  t1=$(now)
  printf '%-34s %8s  %6s ms/saga  (%s sagas/s)\n' "$label" "" \
    "$(node -e "console.log((($t1-$t0)/$n).toFixed(1))")" \
    "$(node -e "console.log(($n/(($t1-$t0)/1000)).toFixed(0))")"
}

echo "=== saga bench (KV=$KV) ==="
printf '%-34s %8s  %6s / %-6s\n' 'endpoint (oha -c 20)' rps 'p50' 'p99(ms)'
oha_row "POST /trips (start)"       POST "/trips" '{"traveler":"b"}'
oha_row "GET /trips/{id}"           GET  "/trips/$SEED"
# (POST /internal/pump is O(running sagas) — not a fixed-cost op, so it's not
#  benched as rps here; see SAGA-BENCH.md takeaways.)
echo "--- full saga, sequential (create + run to terminal) ---"
batch "commit  (3 legs → committed)"    '{"traveler":"b"}'                    100
batch "compensate (car fails → rollback)" '{"traveler":"b","failLeg":"car"}'  100
