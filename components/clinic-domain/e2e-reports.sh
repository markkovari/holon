#!/usr/bin/env bash
# clinic: the reports half
#
# Judges the `reports` part, and judges it on two different things.
#
# BEHAVIOUR, because compiling proves nothing: `cargo component build` passes on a
# crate that implements none of its world, measured twice in this repository. So
# this starts the component and asks it for things.
#
# COMPOSITION, because a hand-rolled `join(",")` and a real CSV encoder both
# answer 200 on a well-behaved row. The component's IMPORTS tell them apart — and
# so does one pet named `Rex, Jr.`, but only the import check also catches a part
# that reimplemented the quoting correctly and still should not have.
#
# The plug chain is not written here. `comp-plug` derives it from the component's
# own imports (`reconciler/src/plug.rs`, which wraps `wac` as a library) — so when a
# part reaches for a capability the world already carries, the gate keeps working
# with no edit. That is the only way an agent can use a capability nobody handed it.
set -uo pipefail

MANIFEST=components/Cargo.toml
HOST="${COMP_HOST:-}"
[ -x "$HOST" ] || { echo "no comp-host at '${HOST}' — the gate cannot run what it built"; exit 1; }

BUILD_LOG="$(mktemp -t clinic-build-XXXX)"
cargo component build --target wasm32-wasip2 --manifest-path "$MANIFEST" \
  -p clinic-domain -p record-store -p id-generate -p auth-guard -p rate-limiter -p audit-log -p search-index -p csv >"$BUILD_LOG" 2>&1 || {
  # From the FIRST error, not the last 25 lines. A candidate's repair prompt is
  # built out of this text, and `tail` hands it the trailing macro notes and
  # "could not compile" while the message, the file and the line scroll off the
  # top. Measured: a part failed three rounds on an E0277 whose location it was
  # never shown, then failed again on a different one it was also never shown.
  echo "the clinic does not compile:"
  awk '/^error/ { seen = 1 } seen' "$BUILD_LOG" | head -45
  rm -f "$BUILD_LOG"; exit 1; }
rm -f "$BUILD_LOG"

T="${CARGO_TARGET_DIR:-components/target}"
# cargo-component writes to wasip1 or wasip2 depending on version; the freshly
# built artifact is the one this gate must judge, so it is found and passed
# explicitly rather than left to the search order.
for d in wasm32-wasip2 wasm32-wasip1; do
  [ -f "$T/$d/debug/clinic_domain.wasm" ] && OUT="$T/$d/debug" && break
done
[ -n "${OUT:-}" ] || { echo "nothing built under $T"; exit 1; }
PLUG="${COMP_PLUG:-reconciler/target/release/comp-plug}"
[ -x "$PLUG" ] || { echo "no comp-plug at '$PLUG' — the gate cannot assemble what it built"; exit 1; }
COMPOSED="$("$PLUG" clinic-domain --dir "$OUT" 2>&1 | tail -1)" || {
  echo "the clinic does not compose: $COMPOSED"; exit 1; }
[ -f "$COMPOSED" ] || { echo "the clinic does not compose: $COMPOSED"; exit 1; }

# --- what it was built out of -------------------------------------------------
#
# Read off the UNCOMPOSED artifact, and the distinction is the whole check. The
# compiler drops an import nothing calls, so a part that formatted its own CSV has
# no `csv:codec` import here however many `use` lines it left behind. The COMPOSED
# component cannot answer this question at all: plugging `csv` SATISFIES the
# import, which removes it just as thoroughly as never calling it did.
have() { wasm-tools component wit "$OUT/clinic_domain.wasm" | grep -q "$1"; }
for want in "csv:codec/codec"; do
  have "$want" || {
    echo "FAILED: the component never calls $want — CSV quoting is a solved problem \
in this repository and that capability is in the world for this part to USE, not to \
reimplement (see CONTRACT.md)"; exit 1; }
done

LOG="$(mktemp -t clinic-log-XXXX)"
PORT=$(( 20000 + RANDOM % 20000 ))
cleanup() { [ -n "${HOST_PID:-}" ] && kill "$HOST_PID" 2>/dev/null; rm -f "$LOG"; }
trap cleanup EXIT

"$HOST" --app clinic --config default-tenant=clinic \
  --component "$COMPOSED" --addr "127.0.0.1:$PORT" >"$LOG" 2>&1 &
HOST_PID=$!

for _ in $(seq 1 60); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)" = "200" ] && break
  sleep 0.5
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
  echo "the component never served /health — it is not up: $(tail -3 "$LOG")"; exit 1; }

B="http://127.0.0.1:$PORT"
fail() { echo "FAILED: $*"; exit 1; }
post() { curl -s -X POST -H 'content-type: application/json' -d "$2" "$B$1"; }
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }

# --- a day with something in it -----------------------------------------------
#
# Built through the OTHER halves' routes, which are already written and in the base
# tree: a report over a fixture nobody booked would not be a report. The name is
# the point — a comma inside a field is what separates a CSV encoder from
# `join(",")`.
OWNER=$(post /api/owners '{"name":"Dana Vance","email":"dana@example.test"}' | field id)
[ -n "$OWNER" ] || fail "could not create an owner to report on"
PET=$(post "/api/owners/$OWNER/pets" '{"name":"Rex, Jr.","species":"dog","born":"2020-05-05"}' | field id)
[ -n "$PET" ] || fail "could not create a pet to report on"
CAT=$(post "/api/owners/$OWNER/pets" '{"name":"Zoe","species":"cat","born":"2021-06-06"}' | field id)
[ -n "$CAT" ] || fail "could not create a second pet to report on"

DAY=2026-09-02
post /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"${DAY}T09:00:00Z\",\"minutes\":30}" >/dev/null
post /api/visits "{\"pet_id\":\"$CAT\",\"vet\":\"vet-b\",\"start\":\"${DAY}T08:00:00Z\",\"minutes\":60}" >/dev/null
post /api/visits "{\"pet_id\":\"$PET\",\"vet\":\"vet-a\",\"start\":\"${DAY}T10:00:00Z\",\"minutes\":15}" >/dev/null

# --- the csv ------------------------------------------------------------------
CODE=$(curl -s -o /dev/null -w '%{http_code}' "$B/api/reports/visits.csv")
[ "$CODE" = "400" ] || fail "a missing day is a 400 (got $CODE)"

CSV=$(curl -s "$B/api/reports/visits.csv?day=$DAY")
python3 - "$CSV" <<'CHECK' || fail "the CSV is not what CONTRACT.md describes: $CSV"
import csv, io, sys
rows = [r for r in csv.reader(io.StringIO(sys.argv[1])) if r]
assert rows, "no rows at all"
assert rows[0] == ["id", "pet_id", "pet_name", "vet", "start", "minutes"], f"header: {rows[0]}"
assert len(rows) == 4, f"three visits and a header make 4 rows, got {len(rows)}"
# The quoting test: every row still has six columns, and the comma survived.
for r in rows[1:]:
    assert len(r) == 6, f"row has {len(r)} columns, not 6 — a comma broke it: {r}"
assert any(r[2] == "Rex, Jr." for r in rows[1:]), f"the comma in the name did not survive: {rows}"
starts = [r[4] for r in rows[1:]]
assert starts == sorted(starts), f"not sorted by start: {starts}"
CHECK

# A day nobody booked is the header alone — not a 404, not an empty body.
EMPTY=$(curl -s "$B/api/reports/visits.csv?day=2026-09-29")
echo "$EMPTY" | head -1 | grep -q '^id,pet_id,pet_name,vet,start,minutes' \
  || fail "an empty day still has its header: $EMPTY"
[ "$(echo "$EMPTY" | grep -c .)" = "1" ] || fail "an empty day has no rows: $EMPTY"

# --- the summary ---------------------------------------------------------------
SUM=$(curl -s "$B/api/reports/summary?day=$DAY")
python3 - "$SUM" <<'CHECK' || fail "the summary is not what CONTRACT.md describes: $SUM"
import json, sys
s = json.loads(sys.argv[1])
assert s["visits"] == 3, s
assert s["minutes"] == 105, s
assert s["by_vet"] == {"vet-a": 2, "vet-b": 1}, s
assert s["by_species"] == {"dog": 2, "cat": 1}, s
CHECK

echo "clinic: the reports half: passed"
