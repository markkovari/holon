# Shared by every triage gate. Sourced, never run.
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
  command -v python3 >/dev/null || {
    echo "no python3 — the gate cannot parse what the component answered"
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
  log="$(mktemp -t triage-build-XXXX)"
  if ! cargo component build --target wasm32-wasip2 --manifest-path components/Cargo.toml \
    -p triage-domain -p record-store -p id-generate -p pii-redact -p fsm-workflow -p csv \
    >"$log" 2>&1; then
    echo "the triage API does not compile:"
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
    [ -f "$t/$d/debug/triage_domain.wasm" ] && OUT="$t/$d/debug" && break
  done
  [ -n "${OUT:-}" ] || { echo "nothing built under $t"; exit 1; }
  COMPOSED="$("$PLUG" triage-domain --dir "$OUT" 2>&1 | tail -1)"
  [ -f "$COMPOSED" ] || { echo "the triage API does not compose: $COMPOSED"; exit 1; }
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
  wasm-tools component wit "$OUT/triage_domain.wasm" | grep -q "$1" || {
    echo "FAILED: the component never calls $1 — $2"
    exit 1
  }
}

# --- run it --------------------------------------------------------------------
gate_serve() {
  LOG="$(mktemp -t triage-log-XXXX)"
  PORT=$(( 20000 + RANDOM % 20000 ))
  "$HOST" --app triage --config default-tenant=triage \
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
get() { curl -s "$B$1"; }
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }

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
  post /test/seed '{}' | python3 -c \
    "import sys,json;[print(i) for i in json.load(sys.stdin).get('report_ids',[])]" 2>/dev/null
}
