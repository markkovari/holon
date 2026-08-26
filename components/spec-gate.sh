#!/usr/bin/env bash
# One gate for every held-out-spec goal: run some named tests, report so the next
# attempt can act on it.
#
#   bash components/spec-gate.sh <crate> <test-target> [test-name ...]
#
# With no test names the whole target runs. With names, only those — which is how a
# goal splits one suite into SEVERAL weighted checks, and that matters more than it
# looks: `checks-runner` scores a candidate `(won * 1000) / total` over the checks,
# so a goal with one check can only ever score 0 or 1000. There is no gradient for a
# generation to climb, and a candidate that got 15 of 19 tests right is indexed
# identically to one that got none. Splitting the suite gives the search a slope.
#
# WHAT THIS FILE IS REALLY ABOUT. ADR-0088: what a gate says is what the next attempt
# reads. So the failing assertions have to reach the model, and two ways of losing
# them are already paid for and avoided here:
#
#   * `error: test failed` is what cargo prints when the SUITE fails, so a `^error`
#     match sends every failing assertion down the compile-error branch and hands the
#     model a summary line instead of the failures. Only rustc writes `error[E…]` and
#     `error: could not compile`.
#   * a third failure shape — a manifest cargo refuses to load — matched neither
#     branch of the first version of this, which printed NOTHING. The run reported
#     `"detail": ""` and the repair attempt was handed nothing to repair against. An
#     empty gate output is the worst value this can produce, so the last branch here
#     is the raw log.
set -uo pipefail
# The components workspace, which is where cargo has a manifest. `$0` is this file,
# so this holds however it was invoked — and every gate that delegates here is under
# it.
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

if echo "$log" | grep -qE "^error\[|^error: could not compile"; then
  # rustc puts the useful part FIRST — the error and its file:line — and a `tail`
  # on a diagnostic gets the trailing macro notes instead.
  echo "$log" | awk '/^error/{p=1} p' | head -n 60
  exit 1
fi

# The summary first: the gate critic prints the first line as its reason, and a
# repair prompt is read from the top down.
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
