#!/usr/bin/env bash
# Every component's held-out-spec gate (`components/*/gate.sh`), one after another, and which failed.
# Needs comp-host, comp-plug and hurl; each gate builds what it hosts (unless already built).
cd "$(dirname "$0")/.." || exit 1

# If wasm artifacts are already built, skip redundant per-gate rebuilds.
if [ -f components/target/wasm32-wasip2/release/ats_domain.wasm ] || [ -f components/target/wasm32-wasip2/release/ats-domain.wasm ]; then
  export GATE_SKIP_BUILD="${GATE_SKIP_BUILD:-1}"
fi
failed=()
for gate in components/*/gate.sh; do
  name=$(basename "$(dirname "$gate")")
  if out=$(bash "$gate" 2>&1); then
    echo "ok   $name"
  else
    echo "FAIL $name"
    echo "$out" | tail -n 25 | sed 's/^/     /'
    failed+=("$name")
  fi
done
[ ${#failed[@]} -eq 0 ] || { echo "${#failed[@]} gate(s) failed: ${failed[*]}"; exit 1; }
