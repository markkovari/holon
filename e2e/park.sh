#!/usr/bin/env bash
# The park app end to end (ADR-0100, ADR-0132): park-gateway + park-store on a real comp-host,
# pumped, delivering to a stub sink. No NATS: tickets live in the app's own keyvalue.
#
#   bash e2e/park.sh                # every scenario once
#   RUNS=3 bash e2e/park.sh         # three times over
#   bash e2e/park.sh reparking      # extra args go to the test binary (a name filter)
#
# CI runs the same suite (`--test park_app` in the reconciler's built-artifacts job).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
RUNS=${RUNS:-1}
cd "$ROOT"
(cd components && cargo build --release --target wasm32-wasip2 -p park-store -p park-gateway)
cargo build --release -p holon-host
# COMP_HOST set: the harness then treats a missing host as a failure, not a skip.
export COMP_HOST="$ROOT/target/release/holon-host"
for run in $(seq "$RUNS"); do
  echo "e2e/park: run $run/$RUNS"
  cargo test --release -p holon-reconciler --test park_app -- --nocapture "$@"
done
