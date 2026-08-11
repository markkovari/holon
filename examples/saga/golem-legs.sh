#!/usr/bin/env bash
# Live proof: a saga whose LEGS are real durable Golem workers.
# Starts Golem (binary), deploys a durable agent, runs the saga on the native
# host with golem-backed legs, and checks the saga committed with golem-issued
# refs — then confirms the leg's Golem worker really advanced its durable state.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
GOLEM="$ROOT/providers/golem-workflow/.bin/golem"
AGENT="$ROOT/providers/golem-workflow/golem-agent"
HOSTBIN="$ROOT/host/target/release/comp-host"
SAGA_WASM="$ROOT/components/target/saga_domain.composed.wasm"
GBASE=http://127.0.0.1:9006
jget() { node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{console.log(eval("(JSON.parse(d))"+process.argv[1]))}catch(e){console.log("")}})' "$1"; }

[ -x "$GOLEM" ] || { echo "FAIL: golem binary missing — run providers/golem-workflow/e2e.sh once"; exit 1; }

# 1. Golem server (--clean for a compatible fresh data dir). Golem 1.5 wipes the
# data dir on --clean but never creates its per-worker kv-store subdir, so the
# worker executor panics ("unable to open database file") on first invoke —
# create it *after* the server boots (before which --clean would wipe it).
if ! curl -sf -o /dev/null -m 2 "$GBASE/" 2>/dev/null; then
  echo "starting golem server..."; "$GOLEM" server run --clean >/tmp/golem-saga.log 2>&1 &
  for _ in $(seq 1 60); do lsof -nP -iTCP:9006 -sTCP:LISTEN >/dev/null 2>&1 && break; sleep 2; done
fi
mkdir -p "$HOME/Library/Application Support/golem/kv-store"

# 2. deploy the durable agent
[ -d "$AGENT" ] || "$GOLEM" new --template rust --component-name book:flight --yes "$AGENT" >/dev/null
GHOST=$( ( cd "$AGENT" && "$GOLEM" build >/dev/null 2>&1; "$GOLEM" deploy -Y 2>&1 ) | grep -oE '[a-z0-9-]+\.localhost(:[0-9]+)?' | head -1 )
GHOST="${GHOST:-golem-agent.localhost:9006}"; case "$GHOST" in *:*) :;; *) GHOST="$GHOST:9006";; esac
echo "golem gateway host: $GHOST"

# warm up: hit the agent route until a real durable worker answers 200 (the
# gateway + worker executor are only truly ready once a worker completes).
for _ in $(seq 1 30); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST -H "Host: $GHOST" "$GBASE/counters/warmup/increment")" = 200 ] && break
  sleep 2
done

# 3. saga app on the native host
just compose-saga >/dev/null 2>&1
( cd host && cargo build --release --bin comp-host >/dev/null 2>&1 )
VET_TENANT=saga "$HOSTBIN" --component "$SAGA_WASM" --addr 127.0.0.1:3016 --kv memory >/tmp/saga-golem-host.log 2>&1 &
HPID=$!; trap 'kill "$HPID" 2>/dev/null || true' EXIT
for _ in $(seq 1 60); do curl -sf -o /dev/null "http://127.0.0.1:3016/" && break; sleep 0.5; done

# 4. run a saga with golem-backed legs
echo "=== running a saga whose legs are Golem workers ==="
ID=$(curl -s -X POST http://127.0.0.1:3016/trips \
  -d "{\"traveler\":\"Ada\",\"golemUrl\":\"$GBASE\",\"golemHost\":\"$GHOST\"}" | jget '.id')
echo "saga: $ID"
curl -s -X POST "http://127.0.0.1:3016/trips/$ID/run" >/dev/null
SAGA=$(curl -s "http://127.0.0.1:3016/trips/$ID")
STATUS=$(echo "$SAGA" | jget '.status')
FLIGHT_REF=$(echo "$SAGA" | jget '.steps[0].ref')
echo "status: $STATUS   flight leg ref: $FLIGHT_REF"

# 5. confirm the flight leg's Golem worker really ran (its durable count advances)
NEXT=$(curl -s -X POST -H "Host: $GHOST" "$GBASE/counters/flight-$ID/increment")
echo "flight worker next increment: $NEXT (saga already booked it once, so > 1)"

if [ "$STATUS" = committed ] && [[ "$FLIGHT_REF" == *golem* ]] && [ "${NEXT:-0}" -gt 1 ]; then
  echo "PASS: saga committed with legs booked by real durable Golem workers"
else
  echo "FAIL: status=$STATUS ref=$FLIGHT_REF next=$NEXT"; exit 1
fi
