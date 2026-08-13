#!/usr/bin/env bash
# Run the slugify demo goal to a pull request.
#
#   bash goal-demo.sh          # smoke test — $0, no model call
#   bash goal-demo.sh real     # the real run — opens a PR (costs a few cents)
#
# Runs under bash no matter what your login shell is, so fish's syntax quirks
# (no `VAR=val cmd`, `()` is command substitution) never come up.
set -euo pipefail
cd "$(dirname "$0")"

export CHECKOUT="/Users/markkovari/DEV/markkovari/agentic-graph-loop-prompt-engineering-playground-generative-ai"
export REPO="markkovari/agentic-graph-loop-prompt-engineering-playground-generative-ai"
export ANTHROPIC_KEY="$HOME/.comp-secrets/anthropic"
export GITHUB_TOKEN="$HOME/.comp-secrets/ghpat"

# Tuning knobs pass through from the environment, so you can isolate problems:
#   env BRANCHES=1 ATTEMPTS=1 bash goal-demo.sh real   # one branch, one attempt
export BRANCHES="${BRANCHES:-4}"
export ROUNDS="${ROUNDS:-1}"
export ATTEMPTS="${ATTEMPTS:-2}"
export MODEL="${MODEL:-claude-haiku-4-5-20251001}"

# Smoke by default; `real` as the first argument does the real run.
if [ "${1:-smoke}" = "real" ]; then
  export SMOKE=0
  echo ">> REAL run: this will call the model and open a PR on $REPO"
else
  export SMOKE=1
  echo ">> SMOKE run: no model call, no cost. Pass 'real' to actually run."
fi

for f in "$ANTHROPIC_KEY" "$GITHUB_TOKEN"; do
  [ -s "$f" ] || { echo "missing or empty secret file: $f"; echo "create it, then re-run"; exit 2; }
done

exec just goal-run
