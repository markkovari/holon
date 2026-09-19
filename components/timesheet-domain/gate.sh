#!/usr/bin/env bash
# timesheet-domain's held-out-spec gate: compose the real capability graph, host it, and
# assert real HTTP behavior — register, login, RBAC and row-level ownership.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

DOMAIN="timesheet-domain"
PORT="${GATE_PORT:-3933}"
HOST="${COMP_HOST:-host/target/release/comp-host}"
PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"

[ -x "$HOST" ] || { echo "no comp-host at '$HOST' — cargo build --release in host/"; exit 1; }
[ -x "$PLUG" ] || { echo "no comp-plug at '$PLUG' — cargo build --release in reconciler/"; exit 1; }

cargo component build --release --manifest-path components/Cargo.toml --target-dir components/target --target wasm32-wasip2 -p "$DOMAIN" -p auth-guard -p policy-guard -p record-store -p audit-log -p rate-limiter >/tmp/gate-$DOMAIN-build.log 2>&1
if [ $? -ne 0 ]; then
  echo "build failed:"; tail -n 60 /tmp/gate-$DOMAIN-build.log; exit 1
fi

ART=$("$PLUG" "$DOMAIN") || { echo "could not compose $DOMAIN"; exit 1; }

"$HOST" --app "$DOMAIN" --component "$ART" --addr "127.0.0.1:$PORT" --kv sqlite \
  --static-dir examples/timesheet/public \
  --config "default-tenant=timesheet" >/tmp/gate-$DOMAIN-host.log 2>&1 &
HOSTPID=$!
trap 'kill $HOSTPID 2>/dev/null; rm -f comp-kv.db comp-kv.db-shm comp-kv.db-wal' EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.25
done

hurl --variable "base=http://127.0.0.1:$PORT" "components/$DOMAIN/scenario.hurl"
STATUS=$?
if [ $STATUS -ne 0 ]; then
  echo "gate FAILED — host log:"; tail -n 60 /tmp/gate-$DOMAIN-host.log
fi
exit $STATUS
