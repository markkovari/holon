#!/usr/bin/env bash
# The code store end to end (ADR-0099, ADR-0134): vcs-gateway + vcs-store on a real comp-host,
# materializing under its --dir. No NATS, no SurrealDB: the store is the app's own keyvalue.
#
#   bash e2e/vcs.sh                 # every scenario once
#   RUNS=3 bash e2e/vcs.sh          # three times over
#   bash e2e/vcs.sh f_real          # extra args go to the test binary (a name filter)
#
# CI runs the same suite (`--test vcs_app` in the reconciler's built-artifacts job).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
RUNS=${RUNS:-1}
cd "$ROOT"
cargo xtask build
cargo build --release -p holon-host
# COMP_HOST set: the harness then treats a missing host as a failure, not a skip.
export COMP_HOST="$ROOT/target/release/holon-host"
for run in $(seq "$RUNS"); do
  echo "e2e/vcs: run $run/$RUNS"
  cargo test --release -p holon-reconciler --test vcs_app -- --nocapture "$@"
done
