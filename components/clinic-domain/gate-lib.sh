# Shared by every clinic gate. Sourced, never run.
#
# The five gates were written by copy-paste and drifted into five copies of the
# same forty lines: build, find the artifact, compose, boot a host, wait for
# /health, and the same curl helpers. That is not just noise — a bug in the shared
# part has to be found and fixed five times, and this file exists because two of
# them already were:
#
#   * `[ "$(pcode … "{\"json\":…}")" = 409 ]` made bash answer `[: too many
#     arguments`, which the run reported as the CANDIDATE failing. Three
#     generations of a real model were judged against a quoting bug.
#   * the build log was handed to the repair prompt as `tail -25`, which on a rustc
#     diagnostic is the trailing macro notes — the error, its message and its
#     file:line have scrolled off the top. A part spent three rounds in each of two
#     runs being asked to fix an error it was never shown.
#
# Both were fixed in five places. The next one gets fixed here.
#
# What a gate still writes for itself is the part that judges: which capabilities
# must appear in the artifact, and what the routes must actually do. That is the
# whole point of the gate and does not belong in a library.

# --- what the harness needs before it can judge anything ----------------------
#
# `$COMP_HOST` and `$COMP_PLUG` are passed in by `holon goal run`: the sandbox
# holds the base tree and nothing else, so neither binary can be found by path.
gate_require_tools() {
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
}

# --- build ---------------------------------------------------------------------
#
# Quiet on success: a check's output IS the feedback a repair reads, and cargo's
# progress drowns the one line that says what went wrong.
#
# On failure, from the FIRST error rather than the last N lines. See the note at
# the top of this file: `tail` hands the repair the part of a rustc diagnostic that
# says nothing.
gate_build() {
  local log
  log="$(mktemp -t clinic-build-XXXX)"
  if ! cargo component build --target wasm32-wasip2 --manifest-path components/Cargo.toml \
    -p clinic-domain -p record-store -p id-generate -p auth-guard -p rate-limiter \
    -p audit-log -p search-index -p csv >"$log" 2>&1; then
    echo "the clinic does not compile:"
    awk '/^error/ { seen = 1 } seen' "$log" | head -45
    rm -f "$log"
    exit 1
  fi
  rm -f "$log"
}

# --- compose -------------------------------------------------------------------
#
# The plug chain is not written here. `comp-plug` derives it from the component's
# own imports (`reconciler/src/plug.rs`, which wraps `wac` as a library), so a part
# that reaches for a capability the world already carries keeps working with no
# edit to any gate. That is the only way an agent can use a capability nobody
# handed it.
#
# cargo-component writes to wasip1 or wasip2 depending on its version; the freshly
# built artifact is the one this gate must judge, so it is found and passed
# explicitly rather than left to the search order.
gate_compose() {
  local t="${CARGO_TARGET_DIR:-components/target}" d
  for d in wasm32-wasip2 wasm32-wasip1; do
    [ -f "$t/$d/debug/clinic_domain.wasm" ] && OUT="$t/$d/debug" && break
  done
  [ -n "${OUT:-}" ] || { echo "nothing built under $t"; exit 1; }
  COMPOSED="$("$PLUG" clinic-domain --dir "$OUT" 2>&1 | tail -1)"
  [ -f "$COMPOSED" ] || { echo "the clinic does not compose: $COMPOSED"; exit 1; }
}

# --- what it was built out of --------------------------------------------------
#
# Read off the UNCOMPOSED artifact, and the distinction is the whole check. The
# compiler drops an import nothing calls, so a part that hand-rolled a capability
# has no import for it however many `use` lines it left behind. The COMPOSED
# component cannot answer this question at all: plugging the provider SATISFIES the
# import, which removes it just as thoroughly as never calling it did — both read
# as absent, and the check would fail every candidate including a correct one.
gate_requires_capability() { # gate_requires_capability <interface> <why>
  wasm-tools component wit "$OUT/clinic_domain.wasm" | grep -q "$1" || {
    echo "FAILED: the component never calls $1 — $2"
    exit 1
  }
}

# --- run it --------------------------------------------------------------------
gate_serve() {
  LOG="$(mktemp -t clinic-log-XXXX)"
  PORT=$(( 20000 + RANDOM % 20000 ))
  "$HOST" --app clinic --config default-tenant=clinic \
    --component "$COMPOSED" --addr "127.0.0.1:$PORT" >"$LOG" 2>&1 &
  HOST_PID=$!
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
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }

# One helper rather than `[ "$(pcode … "{\"json\":…}")" = 409 ]`: that nest of
# quotes inside a command substitution inside a test made bash answer `[: too many
# arguments`, and the run reported it as the candidate failing.
expect_post() { # expect_post <code> <path> <body> <message>
  local want="$1" path="$2" body="$3" msg="$4" got
  got=$(pcode "$path" "$body")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}

expect_get() { # expect_get <code> <url> <message>
  local want="$1" url="$2" msg="$3" got
  got=$(curl -s -o /dev/null -w '%{http_code}' "$url")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}
