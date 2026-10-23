#!/usr/bin/env bash
# Every component's held-out-spec gate (`components/*/gate.sh`), one after another, and which failed.
# Needs comp-host, comp-plug and hurl; each gate builds what it hosts.
cd "$(dirname "$0")/.." || exit 1
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
