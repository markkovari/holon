#!/usr/bin/env bash
# The gate for `card-identify`: run the held-out specification.
#
# A script rather than an argv `cargo test` because the check runs from the tree
# root and the crate lives in the `components/` workspace — and because ADR-0088
# says a gate's output IS the next attempt's prompt, so the failing assertions have
# to reach the model rather than a exit code.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

log=$(cd components && cargo test -p card-identify --test guess 2>&1)
status=$?

if [ $status -eq 0 ]; then
  echo "$log" | tail -n 3
  exit 0
fi

# A compile error and a failed assertion need different halves of the log. rustc
# puts the useful part FIRST (the error and its file:line) and a test harness puts
# it LAST (the failures block), so tailing both ways loses one of them.
# `error: test failed` is what cargo prints when the SUITE fails, so matching a
# bare `^error` sends every failing assertion down the compile-error branch and the
# model is handed a summary line instead of the failures. Match what only rustc
# writes.
if echo "$log" | grep -qE "^error\[|^error: could not compile"; then
  echo "$log" | awk '/^error/{p=1} p' | head -n 60
else
  # The summary FIRST. Both readers of this output take the top: the gate critic
  # prints the first line as its reason, and a repair prompt is read from the top
  # down. Leading with the failures block gave the critic a line reading `---`.
  echo "$log" | grep "^test result" | tail -n 1
  echo
  block=$(echo "$log" | awk '/^failures:$/{p=1} p' | head -n 80)
  # NEVER fall silent. The first version printed nothing at all when cargo failed in
  # a third way — a broken manifest — and an empty gate output is the worst possible
  # feedback: the run reported `"detail": ""` and a repair attempt was handed nothing
  # to repair against (ADR-0088). Anything unrecognised falls through to the raw log.
  if [ -n "$block" ]; then
    echo "$block"
  else
    echo "the suite neither compiled nor reported failures — the whole log follows:"
    echo "$log" | head -n 40
  fi
fi
exit 1
