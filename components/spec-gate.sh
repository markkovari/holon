#!/usr/bin/env bash
# Gate for held-out-spec goals (ADR-0088): run named tests, report what the next attempt needs.
# Test names split one suite into several weighted checks, so checks-runner's score has a gradient.
set -uo pipefail
# cargo needs the components workspace manifest, however this was invoked.
cd "$(dirname "$0")" || exit 1

crate="${1:?usage: spec-gate.sh <crate> <test-target> [test-name ...]}"
target="${2:?a test target is required}"
shift 2

log=$(cargo test -p "$crate" --test "$target" -- "$@" 2>&1)
status=$?

if [ $status -eq 0 ]; then
  echo "$log" | grep "^test result" | tail -n 1
  exit 0
fi

# Not `^error`: cargo prints `error: test failed` for failing tests too.
if echo "$log" | grep -qE "^error\[|^error: could not compile"; then
  # rustc puts the error and its file:line first; a `tail` would get the trailing macro notes.
  echo "$log" | awk '/^error/{p=1} p' | head -n 60
  exit 1
fi

# Summary first: the gate critic uses the first line as its reason.
echo "$log" | grep "^test result" | tail -n 1
echo
block=$(echo "$log" | awk '/^failures:$/{p=1} p' | head -n 80)
if [ -n "$block" ]; then
  echo "$block"
else
  echo "the suite neither compiled nor reported failures — the whole log follows:"
  echo "$log" | head -n 40
fi
exit 1
