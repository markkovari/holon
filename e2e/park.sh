#!/usr/bin/env bash
# A durable record of an outstanding call, end to end (ADR-0100): the real
# stack, then gone.
#
#   bash e2e/park.sh                # every scenario once
#   RUNS=3 bash e2e/park.sh         # three times over, same services
#   bash e2e/park.sh reparking      # extra args go to the test binary (a name filter)
#
# Brings up, in order:
#   - a private `nats-server -js` on a free port with a temp store dir — never
#     the box's :4222, which on a dev machine is often somebody else's NATS
#   - builds park-store and park-gateway (wasm32-wasip2) and comp-host; comp-park is
#     built by cargo for the test itself (CARGO_BIN_EXE_comp-park)
# then runs reconciler/tests/e2e_park.rs, which starts its own comp-park and
# comp-host per scenario, and talks to nothing but the gateway. On exit — pass,
# fail or Ctrl-C — stops NATS and removes the temp dirs.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUNS="${RUNS:-1}"

say() { printf '\033[1;36m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m!! %s\033[0m\n' "$*" >&2; exit 1; }
listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

TMP=$(mktemp -d "${TMPDIR:-/tmp}/park-e2e.XXXXXX")
NATS_PID=""
cleanup() {
  local code=$?
  trap - EXIT INT TERM
  say "tearing down"
  [[ -n "$NATS_PID" ]] && { kill "$NATS_PID" 2>/dev/null || true; wait "$NATS_PID" 2>/dev/null || true; }
  rm -rf "$TMP"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# ---- preflight --------------------------------------------------------------

for tool in nats-server cargo; do
  command -v "$tool" >/dev/null || die "$tool is not on PATH"
done

# ---- build --------------------------------------------------------------------

cd "$ROOT"
say "building park-store + park-gateway (wasm32-wasip2)"
(cd components && cargo build --release --target wasm32-wasip2 -p park-store -p park-gateway)
say "building comp-host"
cargo build --release --manifest-path host/Cargo.toml --bin comp-host
say "building the e2e test (and comp-park with it)"
cargo test --release --manifest-path reconciler/Cargo.toml --test e2e_park --no-run

# ---- private JetStream ----------------------------------------------------------

NATS_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
say "starting a private nats-server -js on :$NATS_PORT"
nats-server -js -a 127.0.0.1 -p "$NATS_PORT" -sd "$TMP/jetstream" >"$TMP/nats.log" 2>&1 &
NATS_PID=$!
for _ in $(seq 50); do listening "$NATS_PORT" && break; sleep 0.1; done
listening "$NATS_PORT" || { cat "$TMP/nats.log"; die "nats-server did not start"; }
export PARK_E2E_NATS_URL="nats://127.0.0.1:$NATS_PORT"

# ---- run ------------------------------------------------------------------------

# COMP_HOST set: the harness then treats a missing host as a failure, not a skip.
export COMP_HOST="$ROOT/host/target/release/comp-host"
status=0
for run in $(seq "$RUNS"); do
  say "run $run/$RUNS: reconciler/tests/e2e_park.rs (NATS $PARK_E2E_NATS_URL)"
  start=$(date +%s)
  if ! cargo test --release --manifest-path reconciler/Cargo.toml --test e2e_park -- --nocapture "$@"; then
    status=1
    say "run $run failed"
    break
  fi
  say "run $run: green in $(( $(date +%s) - start ))s"
done
exit $status
