#!/usr/bin/env bash
# jev-router's held-out-spec gate: compose it with the deterministic mock
# provider, host it, and assert real HTTP behavior for both the "route" and
# "disambiguate" branches — not just that `cargo component check` is happy.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

PORT="${GATE_PORT:-3910}"
HOST="${COMP_HOST:-host/target/release/comp-host}"
PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"

[ -x "$HOST" ] || { echo "no comp-host at '$HOST' — cargo build --release in host/"; exit 1; }
[ -x "$PLUG" ] || { echo "no comp-plug at '$PLUG' — cargo build --release in reconciler/"; exit 1; }

# `jev:decision/decision` has more than one built exporter in this repository
# (`jev-decision`'s own trivial reference provider, the real `typesafe-provider`,
# and this gate's `mock-jev-provider`) — comp-plug resolves an interface to
# whichever built component it scans FIRST, silently. A scoped target dir
# containing only what THIS gate wants, scanned before comp-plug's own
# defaults, guarantees the mock is what gets plugged in — regardless of what
# else has been built into the shared `components/target`.
GATE_DIR="components/target-gate-jev-router"
rm -rf "$GATE_DIR"
cargo component build --release --manifest-path components/Cargo.toml \
  --target-dir "$GATE_DIR" --target wasm32-wasip2 -p jev-router -p mock-jev-provider \
  >/tmp/gate-jev-router-build.log 2>&1
if [ $? -ne 0 ]; then
  echo "build failed:"; tail -n 60 /tmp/gate-jev-router-build.log; exit 1
fi

ART=$("$PLUG" jev-router --dir "$GATE_DIR/wasm32-wasip1/release") || {
  echo "could not compose jev-router"; exit 1
}

# `--config-file` wants one `key = value` per line — the mock script has to be
# a single JSON line here, unlike the multi-line TOML string `apps/jev-router.toml`
# can use.
CONFIG_FILE=$(mktemp)
cat > "$CONFIG_FILE" <<'EOF'
mock-model = mock-jev-1
mock-script = {"rules": [{"when": "printer", "selected": "helpdesk-domain", "confidence": 0.94}, {"when": "appointment", "selected": "clinic-domain", "confidence": 0.9}, {"when": "*", "selected": "helpdesk-domain", "confidence": 0.2, "flat": true}]}
EOF

"$HOST" --app jev-router --component "$ART" --addr "127.0.0.1:$PORT" --kv sqlite \
  --config-file "$CONFIG_FILE" >/tmp/gate-jev-router-host.log 2>&1 &
HOSTPID=$!
trap 'kill $HOSTPID 2>/dev/null; rm -f comp-kv.db comp-kv.db-shm comp-kv.db-wal "$CONFIG_FILE"; rm -rf "$GATE_DIR"' EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.25
done

hurl --variable "base=http://127.0.0.1:$PORT" "components/jev-router/scenario.hurl"
STATUS=$?
if [ $STATUS -ne 0 ]; then
  echo "gate FAILED — host log:"; tail -n 60 /tmp/gate-jev-router-host.log
fi
exit $STATUS
