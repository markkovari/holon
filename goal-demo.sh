#!/usr/bin/env bash
# Usage: bash goal-demo.sh [real]   (smoke by default: no model call; `real` opens a PR)
set -euo pipefail
cd "$(dirname "$0")"

: "${CHECKOUT:?set CHECKOUT to a checkout of the repo the loop should work on}"
: "${REPO:?set REPO to that checkout on the forge as owner/name, e.g. acme/widgets}"
export CHECKOUT REPO
export ANTHROPIC_KEY="$HOME/.comp-secrets/anthropic"
export GITHUB_TOKEN="$HOME/.comp-secrets/ghpat"

export BRANCHES="${BRANCHES:-4}"
export ROUNDS="${ROUNDS:-1}"
export ATTEMPTS="${ATTEMPTS:-2}"
export MODEL="${MODEL:-claude-haiku-4-5-20251001}"
# GOAL_TIMEOUT is the name `.comp/csatapaci.env` uses; the default suits the real API, not slow local models.
export TIMEOUT="${TIMEOUT:-${GOAL_TIMEOUT:-}}"

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

cargo xtask build
(cd host && cargo build --release --bin comp-host)
(cd reconciler && cargo build --release --bins)

# Expand a leading ~ that a quoted env value keeps literal.
args=(--checkout "${CHECKOUT/#\~/$HOME}" --repo "$REPO"
      --anthropic-key "$ANTHROPIC_KEY" --github-token "$GITHUB_TOKEN"
      --branches "$BRANCHES" --rounds "$ROUNDS"
      --model "$MODEL" --attempts "$ATTEMPTS")

# A private BASE_URL also needs COMP_FLEET_ALLOW_PRIVATE_EGRESS=1, deliberately left to the environment.
[ -n "${BASE_URL:-}" ] && args+=(--anthropic-base-url "$BASE_URL")

# A multi-part goal needs the contract registry; a one-part goal never does.
[ -n "${SURREAL_URL:-}" ] && args+=(--surreal-url "$SURREAL_URL")

# The flag takes a file path (a password does not belong in argv); `.comp/csatapaci.env` names it SURREAL_PASSWORD_FILE.
sp="${SURREAL_PASSWORD:-${SURREAL_PASSWORD_FILE:-}}"
[ -n "$sp" ] && args+=(--surreal-password "${sp/#\~/$HOME}")

[ -n "$TIMEOUT" ] && args+=(--timeout "$TIMEOUT")

# A measured run pins this to 1.0: at the default 0.9 a re-run after a harness failure is skipped and reads as a pass.
[ -n "${SKIP_ABOVE:-}" ] && args+=(--skip-above "$SKIP_ABOVE")

[ "${DRY_RUN:-0}" = "1" ] && args+=(--dry-run)
[ "$SMOKE" = "1" ] && args+=(--smoke)

exec ./target/release/comp-goalrun "${args[@]}"
