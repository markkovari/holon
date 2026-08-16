#!/usr/bin/env bash
# pulse app-path bench (docs/apps/REALTIME.md rung 4). The new dimension isn't rps — it's
# CONCURRENT SUSTAINED CONNECTIONS: how many held-open SSE streams the host
# fans a message out to. Plus post/history throughput for reference.
#
# Usage: bench/pulse-bench.sh [memory|nats]
set -euo pipefail
KV="${1:-memory}"
DIR="$(cd "$(dirname "$0")" && pwd)"; ROOT="$(cd "$DIR/.." && pwd)"
ADDR=127.0.0.1:3016; B="http://$ADDR"
BIN="$ROOT/host/target/release/comp-host"; COMP="$ROOT/components/target/pulse_domain.composed.wasm"
jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{const o=JSON.parse(d);console.log(eval("o"+process.argv[1]))})' "$1"; }

args=(--component "$COMP" --addr "$ADDR" --kv "$KV")
[ "$KV" = "nats" ] && args+=(--nats-url 127.0.0.1:4222)
VET_TENANT=pulse "$BIN" "${args[@]}" >/tmp/pulse-bench-host.log 2>&1 &
HPID=$!; trap 'kill $HPID 2>/dev/null || true; rm -f /tmp/pulse-r*.txt' EXIT
for _ in $(seq 1 100); do curl -sf "$B/" >/dev/null 2>&1 && break; sleep 0.1; done

# seed a small, bounded history so the GET row reads ~20 messages, not whatever
# the POST row floods into its (separate) room.
for i in $(seq 1 20); do
  curl -s -X POST "$B/api/rooms/hist/messages" -d '{"user":"seed","text":"hello"}' >/dev/null
done

oha_row() { # name method path [body]
  local name="$1" method="$2" path="$3" body="${4:-}"; local h=()
  [ -n "$body" ] && h=(-H 'content-type: application/json' -d "$body")
  local j; j=$(oha --output-format json -z 8s -c 20 -m "$method" ${h[@]+"${h[@]}"} "$B$path")
  printf '%-30s %8.0f  %6.1f / %-6.1f\n' "$name" \
    "$(echo "$j" | jget '.summary.requestsPerSec')" \
    "$(echo "$j" | jget '.latencyPercentiles.p50*1000')" \
    "$(echo "$j" | jget '.latencyPercentiles.p99*1000')"
}

fanout() { # N concurrent SSE readers all receive one broadcast?
  local n="$1"
  # --max-time (not `timeout`): curl exits cleanly and FLUSHES its stdout to the
  # file; a SIGTERM from `timeout` would drop curl's block-buffered output.
  for i in $(seq 1 "$n"); do curl -sN --max-time 6 "$B/api/rooms/fan/events" >"/tmp/pulse-r$i.txt" 2>/dev/null & done
  sleep 2   # let all readers connect + pin their cursor
  curl -s -X POST "$B/api/rooms/fan/messages" -d '{"user":"bench","text":"BROADCAST-MARKER"}' >/dev/null
  sleep 2.5
  local got; got=$( (grep -l BROADCAST-MARKER /tmp/pulse-r*.txt 2>/dev/null || true) | wc -l | tr -d ' ')
  rm -f /tmp/pulse-r*.txt
  printf '%-30s %s/%s concurrent SSE connections got the broadcast\n' "fan-out (N=$n)" "$got" "$n"
}

echo "=== pulse bench (KV=$KV) ==="
printf '%-30s %8s  %6s / %-6s\n' 'endpoint (oha -c 20)' rps 'p50' 'p99(ms)'
oha_row "POST /messages"   POST "/api/rooms/post/messages" '{"user":"b","text":"hi"}'
oha_row "GET /messages (20)" GET "/api/rooms/hist/messages?after=-1"
echo "--- sustained connections (the point) ---"
fanout 50
fanout 150
