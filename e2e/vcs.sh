#!/usr/bin/env bash
# The code store end to end (ADR-0099): the real stack, then gone.
#
#   bash e2e/vcs.sh                 # every scenario once
#   RUNS=3 bash e2e/vcs.sh          # three times over, same services
#   bash e2e/vcs.sh c_crash         # extra args go to the test binary (a name filter)
#
# Brings up, in order:
#   - SurrealDB from infra/compose.yaml (profile `graph`) as its own compose
#     project, in memory — unless VCS_E2E_SURREAL_URL points at one already
#   - a private `nats-server -js` on a free port with a temp store dir — never
#     the box's :4222, which on a dev machine is often somebody else's NATS
#   - builds vcs-store and vcs-gateway (wasm32-wasip2) and comp-host; comp-vcs is
#     built by cargo for the test itself (CARGO_BIN_EXE_comp-vcs)
# then runs reconciler/tests/e2e_vcs.rs, which starts its own comp-vcs (and
# restarts it, and crashes it on purpose) and comp-host per scenario, and talks
# to nothing but the gateway. On exit — pass, fail or Ctrl-C — stops NATS,
# removes the SurrealDB container and the temp dirs.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
COMPOSE=(docker compose -p holon-vcs-e2e -f "$ROOT/infra/compose.yaml" --profile graph)
SURREAL_PORT=8000   # fixed by infra/compose.yaml
RUNS="${RUNS:-1}"

say() { printf '\033[1;36m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m!! %s\033[0m\n' "$*" >&2; exit 1; }
listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

TMP=$(mktemp -d "${TMPDIR:-/tmp}/vcs-e2e.XXXXXX")
NATS_PID="" SURREAL_UP=""
cleanup() {
  local code=$?
  trap - EXIT INT TERM
  say "tearing down"
  [[ -n "$NATS_PID" ]] && { kill "$NATS_PID" 2>/dev/null || true; wait "$NATS_PID" 2>/dev/null || true; }
  [[ -n "$SURREAL_UP" ]] && "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$TMP"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# ---- preflight --------------------------------------------------------------

for tool in nats-server cargo git; do
  command -v "$tool" >/dev/null || die "$tool is not on PATH"
done
if [[ -z "${VCS_E2E_SURREAL_URL:-}" ]]; then
  command -v docker >/dev/null || die "docker is not on PATH (or set VCS_E2E_SURREAL_URL to a running SurrealDB)"
  docker info >/dev/null 2>&1 || die "docker is not running"
  listening "$SURREAL_PORT" && die "something already listens on 127.0.0.1:$SURREAL_PORT — stop it, or point VCS_E2E_SURREAL_URL at it if it is a SurrealDB you mean to use"
fi

# ---- build --------------------------------------------------------------------

cd "$ROOT"
say "building vcs-store + vcs-gateway (wasm32-wasip2)"
(cd components && cargo build --release --target wasm32-wasip2 -p vcs-store -p vcs-gateway)
say "building comp-host"
cargo build --release --manifest-path host/Cargo.toml --bin comp-host
say "building the e2e test (and comp-vcs with it)"
cargo test --release --manifest-path reconciler/Cargo.toml --test e2e_vcs --no-run

# ---- SurrealDB ------------------------------------------------------------------

if [[ -z "${VCS_E2E_SURREAL_URL:-}" ]]; then
  say "starting SurrealDB (compose profile graph, in memory)"
  SURREAL_UP=1
  "${COMPOSE[@]}" up -d --wait surreal
  export VCS_E2E_SURREAL_URL="127.0.0.1:$SURREAL_PORT"
fi

# ---- private JetStream ----------------------------------------------------------

NATS_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
say "starting a private nats-server -js on :$NATS_PORT"
nats-server -js -a 127.0.0.1 -p "$NATS_PORT" -sd "$TMP/jetstream" >"$TMP/nats.log" 2>&1 &
NATS_PID=$!
for _ in $(seq 50); do listening "$NATS_PORT" && break; sleep 0.1; done
listening "$NATS_PORT" || { cat "$TMP/nats.log"; die "nats-server did not start"; }
export VCS_E2E_NATS_URL="nats://127.0.0.1:$NATS_PORT"

# ---- run ------------------------------------------------------------------------

# COMP_HOST set: the harness then treats a missing host as a failure, not a skip.
export COMP_HOST="$ROOT/host/target/release/comp-host"
status=0
for run in $(seq "$RUNS"); do
  say "run $run/$RUNS: reconciler/tests/e2e_vcs.rs (NATS $VCS_E2E_NATS_URL, SurrealDB $VCS_E2E_SURREAL_URL)"
  start=$(date +%s)
  if ! cargo test --release --manifest-path reconciler/Cargo.toml --test e2e_vcs -- --nocapture "$@"; then
    status=1
    say "run $run failed"
    break
  fi
  say "run $run: green in $(( $(date +%s) - start ))s"
done
exit $status
