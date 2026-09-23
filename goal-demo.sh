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

# The repository the loop WORKS ON — deliberately not this one, and deliberately
# not hardcoded. `README.md` offers `bash goal-demo.sh real` as the one command
# that takes a goal to a pull request, and it named one person's home directory,
# so it was one command that worked for exactly one person.
: "${CHECKOUT:?set CHECKOUT to a checkout of the repo the loop should work on}"
: "${REPO:?set REPO to that checkout on the forge as owner/name, e.g. acme/widgets}"
export CHECKOUT REPO
export ANTHROPIC_KEY="$HOME/.comp-secrets/anthropic"
export GITHUB_TOKEN="$HOME/.comp-secrets/ghpat"

# Tuning knobs pass through from the environment, so you can isolate problems:
#   env BRANCHES=1 ATTEMPTS=1 bash goal-demo.sh real   # one branch, one attempt
export BRANCHES="${BRANCHES:-4}"
export ROUNDS="${ROUNDS:-1}"
export ATTEMPTS="${ATTEMPTS:-2}"
export MODEL="${MODEL:-claude-haiku-4-5-20251001}"
# The branch budget, read under BOTH names: TIMEOUT, and GOAL_TIMEOUT, which is
# what `.comp/csatapaci.env` sets.
#
# This script exported every other knob and not this one, so the one command the
# README offers could not be given a timeout at all — and the default is sized for
# the real API (median 64s a call), not for a local model at 417s or the Claude CLI
# shim at 90-135s. A branch over budget does not say so: the client hangs up, the
# host logs `IncompleteMessage`, and the run reports `error sending request for url
# .../run`, which reads as a fleet fault. Seven branches died that way across two
# paid runs before the number was the suspect.
#
# Unset stays unset, so the flag is simply absent and comp-goalrun's own default
# applies — this passes a value through, it does not invent one.
export TIMEOUT="${TIMEOUT:-${GOAL_TIMEOUT:-}}"

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

# ---- build, then run the goal ------------------------------------------------
#
# This used to end in `exec just goal-run`, and the Justfile is gone (`cargo xtask`
# replaced it), so the recipe's body lives here now. Same three builds: the WASM
# components the fleet runs, the host that runs them, and the loop's own binaries.
cargo xtask build
(cd host && cargo build --release --bin comp-host)
(cd reconciler && cargo build --release --bins)

# Expand a leading ~ that a quoted env value keeps literal.
args=(--checkout "${CHECKOUT/#\~/$HOME}" --repo "$REPO"
      --anthropic-key "$ANTHROPIC_KEY" --github-token "$GITHUB_TOKEN"
      --branches "$BRANCHES" --rounds "$ROUNDS"
      --model "$MODEL" --attempts "$ATTEMPTS")

# BASE_URL points inference somewhere other than the real API — the shim being the
# reason it exists. A private address also needs COMP_FLEET_ALLOW_PRIVATE_EGRESS=1,
# which is inherited from the environment and deliberately not set here.
[ -n "${BASE_URL:-}" ] && args+=(--anthropic-base-url "$BASE_URL")

# A multi-part goal needs the contract registry; a one-part goal never does.
[ -n "${SURREAL_URL:-}" ] && args+=(--surreal-url "$SURREAL_URL")

# Both names, for the same reason as the timeout. The flag takes a PATH, and
# `.comp/csatapaci.env` calls it SURREAL_PASSWORD_FILE — saying so, because a
# password does not belong in argv. Reading only SURREAL_PASSWORD meant sourcing
# that file passed `--surreal-url` and no password, which against a root-auth
# database is a trace and a pool that write nothing, and the run reports its drops
# and carries on green.
sp="${SURREAL_PASSWORD:-${SURREAL_PASSWORD_FILE:-}}"
[ -n "$sp" ] && args+=(--surreal-password "${sp/#\~/$HOME}")

[ -n "$TIMEOUT" ] && args+=(--timeout "$TIMEOUT")

# A measured run pins this to 1.0 — never skip. At the default 0.9 a re-run after a
# HARNESS failure is skipped as work already done, which reads as a pass.
[ -n "${SKIP_ABOVE:-}" ] && args+=(--skip-above "$SKIP_ABOVE")

[ "${DRY_RUN:-0}" = "1" ] && args+=(--dry-run)
[ "$SMOKE" = "1" ] && args+=(--smoke)

exec ./reconciler/target/release/comp-goalrun "${args[@]}"
