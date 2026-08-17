#!/usr/bin/env bash
# triage: the digest half
#
# Judges the `digest` part on behaviour AND on composition. A hand-rolled
# `join(",")` and a real CSV encoder both answer 200 on a well-behaved row; the
# component's IMPORTS tell them apart, and so does the seeded report titled
# `Login fails, silently` — but only the import check also catches a part that
# reimplemented the quoting correctly and still should not have.
set -uo pipefail
# shellcheck source=components/triage-domain/gate-lib.sh
. components/triage-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "csv:codec/codec" \
  "CSV quoting is a solved problem in this repository and that capability is in the world for this part to USE, not to reimplement (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve

# Two ids out of the fixture, WITHOUT `mapfile`: that is bash 4, and macOS ships
# bash 3.2 — the goal invokes these gates as `["bash", …]`, so a bash-4 builtin here
# fails every branch of a run with `mapfile: command not found`, three lines before
# anything is judged.
SEEDED=$(gate_seed)
A=$(printf '%s
' "$SEEDED" | sed -n 1p)
B_ID=$(printf '%s
' "$SEEDED" | sed -n 2p)
[ -n "$A" ] && [ -n "$B_ID" ] || fail "the fixture did not seed two reports — POST /test/seed answered: $(post /test/seed '{}')"

# --- day is required -----------------------------------------------------------
expect_get 400 "/api/digest" "a missing day is a 400"
expect_get 400 "/api/digest?day=not-a-date" "an unparseable day is a 400"
expect_get 400 "/api/digest.csv" "a missing day is a 400 for the CSV too"

# --- the JSON digest -----------------------------------------------------------
#
# The fixture writes both reports at 2026-08-17T09:00:00Z.
python3 - "$(get '/api/digest?day=2026-08-17')" <<'PY' || fail "the JSON digest is wrong"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("day")=="2026-08-17", d
assert d.get("total",0)>=2, d
bs,bc=d.get("by_state"),d.get("by_component")
assert isinstance(bs,dict) and isinstance(bc,dict), d
assert bs.get("open",0)>=2, ("both seeded reports are open", bs)
# Only states/components that OCCUR are present — no zero-filled keys.
assert all(v>0 for v in bs.values()), bs
assert all(v>0 for v in bc.values()), bc
assert {"auth","billing"} <= set(bc), bc
assert "open_high" in d, d
PY

# A day with nothing in it is an empty digest, not a 404.
python3 - "$(get '/api/digest?day=1999-01-01')" <<'PY' || fail "an empty day must still be a digest"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("total")==0, d
assert d.get("by_state")=={} and d.get("by_component")=={}, d
PY

# --- the CSV -------------------------------------------------------------------
#
# Parsed with a real parser and the columns counted. `Login fails, silently` is
# seeded precisely so that joining with commas produces a row with six fields.
# The document goes in as an ARGUMENT, not on stdin: `python3 -` reads its PROGRAM
# from stdin, so a heredoc and a pipe are the same channel and the heredoc wins —
# `csv.reader(sys.stdin)` then reads nothing and the gate fails a correct candidate
# with "no rows at all".
CSV=$(get '/api/digest.csv?day=2026-08-17')
python3 - "$CSV" <<'PY' || fail "the CSV is wrong"
import csv,io,sys
rows=list(csv.reader(io.StringIO(sys.argv[1])))
assert rows, "no rows at all"
assert rows[0]==["id","title","component","state","severity"], ("header", rows[0])
body=[r for r in rows[1:] if r]
assert len(body)>=2, ("one row per report", rows)
for r in body:
    assert len(r)==5, ("every row has five columns — a comma in a title must be quoted", r)
titles=[r[1] for r in body]
assert "Login fails, silently" in titles, ("the comma-bearing title must survive intact", titles)
# severity is absent on a seeded report, and absent means EMPTY, not "null".
sev=[r[4] for r in body]
assert all(s!="null" for s in sev), ("an absent severity is an empty field", sev)
PY

# The content type has to be text/csv, or a browser and a parser both see JSON.
CT=$(curl -s -o /dev/null -w '%{content_type}' "$B/api/digest.csv?day=2026-08-17")
case "$CT" in
  text/csv*) ;;
  *) fail "the CSV must be served as text/csv, not '$CT' — use Reply::raw" ;;
esac

# An empty day is the header alone.
python3 - "$(get '/api/digest.csv?day=1999-01-01')" <<'PY' || fail "an empty day is the header alone"
import csv,io,sys
rows=[r for r in csv.reader(io.StringIO(sys.argv[1])) if r]
assert len(rows)==1 and rows[0][0]=="id", rows
PY

echo "triage: the digest half: passed"
