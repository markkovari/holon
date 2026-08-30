# Shared by every gate in this repository. Sourced, never run.
#
# A gate says which app it is judging by setting three variables before it sources
# this, and nothing else about an app is known here:
#
#   GATE_CRATE   the crate under test, e.g. `triage-domain`
#   GATE_APP     the app name the host runs it as, e.g. `triage`
#   GATE_PKGS    every `-p` cargo needs, as one string
#
# Optional, appended to by helpers rather than set by hand: `GATE_CONFIG` (extra
# `--config key=value` pairs) and `GATE_EGRESS` (authorities the component may
# reach — DEFAULT-DENY, so an app that calls out gets nothing without one).
#
# The same library `clinic-domain` grew, for the same reason: four gates that each
# build, compose, boot a host, wait for /health and curl things is four copies of
# forty lines, and a bug in the shared part has to be found and fixed four times.
# Two of the clinic's were, and both are avoided here by construction:
#
#   * `[ "$(pcode … "{\"json\":…}")" = 409 ]` made bash answer `[: too many
#     arguments`, which the run reported as the CANDIDATE failing. Three generations
#     of a real model were judged against a quoting bug. Hence `expect_post`.
#   * the build log was handed to the repair prompt as `tail -25`, which on a rustc
#     diagnostic is the trailing macro notes — the error and its file:line have
#     scrolled off the top. Hence `awk` from the FIRST error.
#
# What a gate still writes for itself is the part that judges: which capability must
# appear in the artifact, and what the routes must actually do.

# --- what the harness needs before it can judge anything ----------------------
#
# `$COMP_HOST` and `$COMP_PLUG` are passed in by `holon goal run`: the sandbox holds
# the base tree and nothing else, so neither binary can be found by path.
gate_require_tools() {
  : "${GATE_CRATE:?a gate must set GATE_CRATE before sourcing gate-lib.sh}"
  : "${GATE_APP:?a gate must set GATE_APP before sourcing gate-lib.sh}"
  : "${GATE_PKGS:?a gate must set GATE_PKGS before sourcing gate-lib.sh}"
  # cargo names the artifact after the crate with dashes turned into underscores.
  GATE_WASM="${GATE_CRATE//-/_}"
  HOST="${COMP_HOST:-}"
  [ -x "$HOST" ] || {
    echo "no comp-host at '${HOST}' — the gate cannot run what it built"
    exit 1
  }
  PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"
  [ -x "$PLUG" ] || {
    echo "no comp-plug at '$PLUG' — the gate cannot assemble what it built"
    exit 1
  }
  command -v wasm-tools >/dev/null || {
    echo "no wasm-tools — the gate cannot read what the component imports"
    exit 1
  }
  FIELD="${COMP_FIELD:-reconciler/target/release/comp-field}"
  [ -x "$FIELD" ] || {
    echo "no comp-field at '$FIELD' — the gate cannot parse what the component answered"
    echo "  cargo build --release --manifest-path reconciler/Cargo.toml --bin comp-field"
    exit 1
  }
}

# --- build ---------------------------------------------------------------------
#
# Quiet on success: a check's output IS the feedback a repair reads, and cargo's
# progress drowns the one line that says what went wrong. On failure, from the FIRST
# error — see the note at the top of this file.
gate_build() {
  local log
  log="$(mktemp -t gate-build-XXXX)"
  # shellcheck disable=SC2086 -- GATE_PKGS is a list of flags, word splitting is the point
  if ! cargo component build --target wasm32-wasip2 --manifest-path components/Cargo.toml \
    $GATE_PKGS \
    >"$log" 2>&1; then
    echo "$GATE_CRATE does not compile:"
    awk '/^error/ { seen = 1 } seen' "$log" | head -45
    rm -f "$log"
    exit 1
  fi
  rm -f "$log"
}

# --- compose -------------------------------------------------------------------
#
# The plug chain is not written here. `comp-plug` derives it from the component's own
# imports, so a part that reaches for a capability the world already carries keeps
# working with no edit to any gate. That is the only way an agent can use a
# capability nobody handed it.
gate_compose() {
  local t="${CARGO_TARGET_DIR:-components/target}" d
  for d in wasm32-wasip2 wasm32-wasip1; do
    [ -f "$t/$d/debug/$GATE_WASM.wasm" ] && OUT="$t/$d/debug" && break
  done
  [ -n "${OUT:-}" ] || { echo "nothing built under $t"; exit 1; }
  COMPOSED="$("$PLUG" "$GATE_CRATE" --dir "$OUT" 2>&1 | tail -1)"
  [ -f "$COMPOSED" ] || { echo "$GATE_CRATE does not compose: $COMPOSED"; exit 1; }
}

# --- what it was built out of --------------------------------------------------
#
# Read off the UNCOMPOSED artifact, and the distinction is the whole check. The
# compiler drops an import nothing calls, so a part that hand-rolled a capability has
# no import for it however many `use` lines it left behind. The COMPOSED component
# cannot answer this at all: plugging the provider SATISFIES the import, which
# removes it as thoroughly as never calling it did — both read as absent, and the
# check would fail every candidate including a correct one.
gate_requires_capability() { # gate_requires_capability <interface> <why>
  wasm-tools component wit "$OUT/$GATE_WASM.wasm" | grep -q "$1" || {
    echo "FAILED: the component never calls $1 — $2"
    exit 1
  }
}

# The same claim about a component that is NOT the one under test.
#
# `gate_requires_capability` reads the crate the gate is named for, which is right
# when that crate does the calling. It is wrong the moment a gate drives a stack: a
# probe over a fan-out imports the fan-out, and the interface that matters is one
# the FAN-OUT imports. Asserting on the probe reports "never calls mail:send" about
# a component that was never supposed to.
gate_component_requires() { # gate_component_requires <crate> <interface> <why>
  local art="$OUT/${1//-/_}.wasm"
  [ -f "$art" ] || {
    echo "FAILED: no artifact for $1 at $art — is it in GATE_PKGS?"
    exit 1
  }
  wasm-tools component wit "$art" | grep -q "$2" || {
    echo "FAILED: $1 never calls $2 — $3"
    exit 1
  }
}

# --- reaching the model, through the shim --------------------------------------
#
# An app whose gate makes a real inference call needs three things, and getting any
# one of them wrong fails as a network error two minutes into the call rather than
# as a line saying what is missing. So it is one helper, called before `gate_serve`:
#
#   * the provider's base URL, pointed at `tools/claude-shim.mjs`;
#   * its timeout, because `claude -p` has a median near 130s and a tail past 300s —
#     the provider's default of 180 has already killed a whole part of a real run;
#   * egress, twice over: the authority by name (default-deny) and the private-address
#     opt-out, since 127.0.0.1 is exactly what the host refuses by default.
#
# No API key: a missing secret is not an error in `anthropic-provider` (`api_key()`
# returns `none` and the header is simply absent), and the shim ignores auth anyway.
gate_shim_config() {
  local url="${SHIM_URL:-http://127.0.0.1:8787}" auth
  auth="${url#http://}"
  # A GET, which the shim answers 404 without spawning anything. Probing
  # `/v1/messages` for real would spend a `claude -p` call on liveness.
  if [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$url/")" != "404" ]; then
    echo "no shim at $url — start it with \`just claude-shim &\` before this gate"
    exit 1
  fi
  GATE_CONFIG="${GATE_CONFIG:-} --config anthropic:base-url=$url --config anthropic:timeout=540"
  GATE_EGRESS="${GATE_EGRESS:-} --egress $auth"
  GATE_PRIVATE_EGRESS=--allow-private-egress
}

# --- run it --------------------------------------------------------------------
gate_serve() {
  LOG="$(mktemp -t gate-log-XXXX)"
  PORT=$(( 20000 + RANDOM % 20000 ))
  # shellcheck disable=SC2086 -- the three GATE_* vars are flag lists, unquoted on purpose
  "$HOST" --app "$GATE_APP" --config "default-tenant=$GATE_APP" \
    ${GATE_CONFIG:-} ${GATE_EGRESS:-} ${GATE_PRIVATE_EGRESS:-} \
    --component "$COMPOSED" --addr "127.0.0.1:$PORT" >"$LOG" 2>&1 &
  HOST_PID=$!
  # Off the job table, so killing it in `gate_cleanup` does not make the shell print
  # `Terminated: 15` — which a gate that restarts the host does on a PASSING run, and
  # which lands in the branch's feedback looking exactly like the failure it is not.
  disown "$HOST_PID" 2>/dev/null || true
  B="http://127.0.0.1:$PORT"
  local _
  for _ in $(seq 1 60); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' "$B/health" 2>/dev/null)" = "200" ] && break
    sleep 0.5
  done
  [ "$(curl -s -o /dev/null -w '%{http_code}' "$B/health")" = "200" ] || {
    echo "the component never served /health — it is not up: $(tail -3 "$LOG")"
    exit 1
  }
}

gate_cleanup() {
  [ -n "${HOST_PID:-}" ] && kill "$HOST_PID" 2>/dev/null
  rm -f "$LOG"
}

# --- asking it things ----------------------------------------------------------
fail() { echo "FAILED: $*"; exit 1; }
post() { curl -s -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
pcode() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
get() { curl -s "$B$1"; }
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
# One field out of a JSON body. Was `python3 -c "import sys,json;…"`, at 14.9 ms of
# interpreter start-up per call — measured — against 1.6 ms for the binary. There are
# 106 call sites, and the loop re-runs every gate per candidate per attempt, so the
# difference is multiplied by every branch of every graph it explores.
field() { "$FIELD" "$1" 2>/dev/null; }

# One helper rather than `[ "$(pcode … "{\"json\":…}")" = 409 ]`: that nest of quotes
# inside a command substitution inside a test made bash answer `[: too many
# arguments`, and the run reported it as the candidate failing.
expect_post() { # expect_post <code> <path> <body> <message>
  local want="$1" path="$2" body="$3" msg="$4" got
  got=$(pcode "$path" "$body")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}

expect_get() { # expect_get <code> <path> <message>
  local want="$1" path="$2" msg="$3" got
  got=$(curl -s -o /dev/null -w '%{http_code}' "$B$2")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}

# Seed the fixture and echo the report ids, one per line.
#
# Every gate needs reports and no gate may depend on `intake` existing: all three
# parts are written at the same time by different agents.
gate_seed() {
  post /test/seed '{}' | "$FIELD" --list report_ids 2>/dev/null
}
