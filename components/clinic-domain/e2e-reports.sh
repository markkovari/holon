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
# shellcheck source=components/clinic-domain/gate-lib.sh
. components/clinic-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "csv:codec/codec" \
  "CSV quoting is a solved problem in this repository and that capability is in the world for this part to USE, not to reimplement (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve

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
