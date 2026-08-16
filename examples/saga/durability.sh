#!/usr/bin/env bash
# Durability proof (docs/apps/SAGA.md rung 3): a saga's state lives entirely in NATS
# JetStream KV (records + fsm), so it survives the host process dying mid-flight.
# Start a saga, advance it one leg, KILL the host, restart it, and show the saga
# resumes exactly where it left off — then pump it to commit.
#
# Prereq: NATS on :4222 (docker compose -f infra/compose.yaml up -d nats) and a
# composed saga app + built host (`just compose-saga` + host build).
set -euo pipefail
cd "$(dirname "$0")/../.."
ADDR=127.0.0.1:3013; B="http://$ADDR"
BIN=host/target/release/comp-host
COMP=components/target/saga_domain.composed.wasm
jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{console.log(eval("(JSON.parse(d))"+process.argv[1]))}catch(e){console.log("")}})' "$1"; }

nc -z 127.0.0.1 4222 2>/dev/null || { echo "FAIL: NATS not on :4222 (docker compose -f infra/compose.yaml up -d nats)"; exit 1; }
[ -f "$COMP" ] || { echo "FAIL: composed wasm missing (just compose-saga)"; exit 1; }

start() { VET_TENANT=saga "$BIN" --component "$COMP" --addr "$ADDR" --kv nats --nats-url 127.0.0.1:4222 >/tmp/saga-durable.log 2>&1 & echo $!; }
wait_up() { for _ in $(seq 1 60); do curl -sf "$B/" >/dev/null 2>&1 && return; sleep 0.2; done; echo "FAIL: host not up"; exit 1; }

PID=$(start); wait_up
ID=$(curl -s -X POST "$B/trips" -d '{"traveler":"Resumer"}' | jget '.id')
echo "started saga $ID"
curl -s -X POST "$B/internal/pump" >/dev/null   # book one leg
BEFORE=$(curl -s "$B/trips/$ID"); echo "before restart: status=$(echo "$BEFORE" | jget '.status') flight=$(echo "$BEFORE" | jget '.steps[0].state')"

echo "--- killing host (pid $PID) ---"; kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true
PID=$(start); wait_up   # restart against the same NATS
AFTER=$(curl -s "$B/trips/$ID")
FL=$(echo "$AFTER" | jget '.steps[0].state'); ST=$(echo "$AFTER" | jget '.status')
echo "after restart:  status=$ST flight=$FL"

for _ in 1 2 3 4; do curl -s -X POST "$B/internal/pump" >/dev/null; done
FINAL=$(curl -s "$B/trips/$ID" | jget '.status')
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true

echo "final status: $FINAL"
if [ "$FL" = "booked" ] && [ "$ST" = "running" ] && [ "$FINAL" = "committed" ]; then
  echo "PASS: saga survived the restart (flight stayed booked) and resumed to committed"
else
  echo "FAIL: expected flight=booked/status=running after restart, committed at end"; exit 1
fi
