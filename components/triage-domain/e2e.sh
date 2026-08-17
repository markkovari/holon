#!/usr/bin/env bash
# triage: the whole API
#
# The COMPOSITION gate. Every part passes its own gate against the fixture; this is
# the one none of them can pass alone, because it drives a report the whole way
# through all three:
#
#   intake writes it  ->  workflow moves it and assigns severity  ->  digest counts it
#
# That chain is why this goal has three parts and not two. `digest` can only be right
# if `intake` stored the contract's shape and `workflow` updated the document rather
# than only the fsm instance. Each of those is a plausible local success that shows up
# nowhere until the halves meet.
set -uo pipefail
# shellcheck source=components/triage-domain/gate-lib.sh
. components/triage-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

# All three, in one place: a candidate that dropped one capability fails here even if
# the part that owns it was never the one judged.
gate_requires_capability "pii:redact/redactor" "intake must mask the body with pii:redact"
gate_requires_capability "fsm:workflow/engine" "workflow must validate moves with the fsm engine"
gate_requires_capability "csv:codec/codec"     "digest must format the CSV with csv:codec"

trap gate_cleanup EXIT
gate_serve

# --- one report, all the way through -------------------------------------------
DAY=2026-08-17
RESP=$(post /api/reports '{"title":"Totals drift, badly","body":"ping me on +1 555 010 0199","component":"billing"}')
ID=$(printf '%s' "$RESP" | field id)
[ -n "$ID" ] || fail "intake did not create a report: $RESP"

# A phone number is PII too, so intake masks more than emails.
#
# ELEVEN digits with a `+`, not `555-0100`. The scanner wants 10-15 digits and
# either a leading `+` or a NANP-looking span, so a 7-digit local number is not a
# phone number by its definition — asserting on one would have this gate "prove"
# that masking was broken when it was working exactly as specified.
case "$(get "/api/reports/$ID")" in
  *0199*) fail "the reporter's phone number was stored verbatim" ;;
esac
case "$(get "/api/reports/$ID")" in
  *'[PHONE]'*) ;;
  *) fail "the phone number was not masked with pii:redact's placeholder: $(get "/api/reports/$ID")" ;;
esac

# workflow moves it and assigns a severity
expect_post 200 "/api/reports/$ID/transition" '{"event":"triage","severity":"high"}' \
  "workflow could not triage a report intake had just created"

# The queue is workflow's view; it must contain the report intake wrote.
python3 - "$(get '/api/queue')" "$ID" <<'PY' || fail "the queue does not contain the triaged report"
import json,sys
d=json.loads(sys.argv[1]); i=sys.argv[2]
q={r["id"]: r for r in d["queue"]}
assert i in q, ("workflow's queue is missing intake's report", list(q))
assert q[i].get("severity")=="high", q[i]
PY

# --- digest sees what the other two did ----------------------------------------
#
# This is the assertion that needs all three parts to agree. `open_high` counts
# reports with severity high that are not closed — a number that exists only because
# intake wrote the document, workflow put `high` on it, and digest read it back.
python3 - "$(get "/api/digest?day=$DAY")" <<'PY' || fail "the digest does not reflect the triaged report"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("open_high",0)>=1, ("a high-severity open report was triaged; the digest missed it", d)
assert d.get("by_component",{}).get("billing",0)>=1, d
assert d.get("by_state",{}).get("triaged",0)>=1, ("workflow moved a report to triaged and the document did not follow", d)
PY

# And the CSV carries the severity workflow assigned, in the row for that report.
# Argument, not stdin: `python3 -` takes its program from stdin, so a pipe into a
# heredoc silently gives the reader nothing.
python3 - "$ID" "$(get "/api/digest.csv?day=$DAY")" <<'PY' || fail "the CSV does not carry what workflow assigned"
import csv,io,sys
want=sys.argv[1]
rows=[r for r in csv.reader(io.StringIO(sys.argv[2])) if r]
head,body=rows[0],rows[1:]
assert head==["id","title","component","state","severity"], head
row=[r for r in body if r[0]==want]
assert row, ("the report is missing from the CSV", want, [r[0] for r in body])
r=row[0]
assert len(r)==5, r
assert r[3]=="triaged", ("state", r)
assert r[4]=="high", ("severity", r)
# The comma-bearing title from the fixture still has to survive alongside it.
titles=[x[1] for x in body]
assert "Totals drift, badly" in titles, ("a comma in a title must be quoted, not split", titles)
PY

# --- closing takes it out of the queue, and the digest agrees -------------------
expect_post 200 "/api/reports/$ID/transition" '{"event":"fix"}'   "triaged -> fixed"
expect_post 200 "/api/reports/$ID/transition" '{"event":"close"}' "fixed -> closed"
python3 - "$(get '/api/queue')" "$ID" <<'PY' || fail "a closed report is still in the queue"
import json,sys
d=json.loads(sys.argv[1]); i=sys.argv[2]
assert i not in [r["id"] for r in d["queue"]], "closed reports are not queued"
PY
python3 - "$(get "/api/digest?day=$DAY")" <<'PY' || fail "the digest still counts the closed report as open_high"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("by_state",{}).get("closed",0)>=1, d
PY

# A closed report no longer blocks a new one with the same title+component: the bug
# came back. Intake's rule, checked here because it depends on workflow having closed
# it — neither part can assert this alone.
expect_post 201 /api/reports '{"title":"Totals drift, badly","body":"again","component":"billing"}' \
  "a closed report must not block a new report with the same title"

echo "triage: every route behaved"
