#!/usr/bin/env bash
# triage: the workflow half
#
# Judges the `workflow` part on behaviour AND on composition. A hand-rolled
# transition table and a real state machine both answer 200 on a legal move; the
# component's IMPORTS tell them apart, and an illegal move has to report the CURRENT
# state, which is what the fsm's own error carries.
#
# Reports come from the fixture, not from `intake`: all three parts are written at the
# same time by different agents, so a gate that needed another part's work would judge
# the wrong thing.
set -uo pipefail
# shellcheck source=components/triage-domain/gate-lib.sh
. components/triage-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "fsm:workflow/engine" \
  "the lifecycle is a DEFINITION the fsm engine validates, not a ladder of string comparisons — that capability is in the world for this part to USE (see CONTRACT.md)"

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

# --- what a bad request is ----------------------------------------------------
expect_post 400 "/api/reports/$A/transition" '{"event":"explode"}' "an unknown event is a 400"
expect_post 400 "/api/reports/$A/transition" '{}' "a missing event is a 400"
expect_post 404 "/api/reports/nope/transition" '{"event":"close"}' "an unknown report is a 404"

# --- triage requires a severity ------------------------------------------------
expect_post 400 "/api/reports/$A/transition" '{"event":"triage"}' \
  "the triage event requires a severity"
expect_post 400 "/api/reports/$A/transition" '{"event":"triage","severity":"urgent"}' \
  "a severity outside low/medium/high is a 400"

# --- the legal path ------------------------------------------------------------
RESP=$(post "/api/reports/$A/transition" '{"event":"triage","severity":"high"}')
python3 - "$RESP" <<'PY' || fail "triage must answer with the new state and the severity"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("state")=="triaged", d
assert d.get("severity")=="high", d
PY

# The DOCUMENT must have moved too, not just the fsm instance: `digest` reads the
# document and must not have to ask the fsm what state a report is in.
#
# Read through the SCAFFOLD's `/test/report/{id}`, not `GET /api/reports/{id}` —
# that route belongs to `intake`, which is a stub while this part is judged alone.
# This gate asked intake for the document, got `{"error":"not_implemented"}`, and
# reported it as `workflow` having failed to move it.
python3 - "$(get "/test/report/$A")" <<'PY' || fail "the report document did not follow the fsm"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("state")=="triaged", ("the document still says", d.get("state"), d)
assert d.get("severity")=="high", d
PY

# --- the illegal move, and what it must say ------------------------------------
#
# `fix` is legal only from `triaged`. This report is triaged, so `fix` works; a
# second `triage` does not, and the 409 has to name the state it is actually in.
RESP=$(post "/api/reports/$A/transition" '{"event":"triage","severity":"low"}')
CODE=$(pcode "/api/reports/$A/transition" '{"event":"triage","severity":"low"}')
[ "$CODE" = "409" ] || fail "triaging an already-triaged report is a 409 (got $CODE)"
python3 - "$RESP" <<'PY' || fail "a 409 must carry the current state, which the fsm error provides"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("error")=="illegal", d
assert d.get("state")=="triaged", ("the 409 must name the current state", d)
PY

# open -> fixed is not a legal jump
expect_post 409 "/api/reports/$B_ID/transition" '{"event":"fix"}' \
  "open cannot jump straight to fixed"

# --- terminal really is terminal ------------------------------------------------
expect_post 200 "/api/reports/$B_ID/transition" '{"event":"close"}' "open can be closed (not a bug)"
expect_post 409 "/api/reports/$B_ID/transition" '{"event":"triage","severity":"low"}' \
  "a closed report is terminal and accepts nothing"

# --- the queue -----------------------------------------------------------------
#
# Ordering is the whole check: severity first, no-severity last, older first inside a
# severity. And a closed report is not in the queue at all.
post "/api/reports/$A/transition" '{"event":"fix"}' >/dev/null
python3 - "$(get '/api/queue')" "$A" "$B_ID" <<'PY' || fail "the queue is wrong"
import json,sys
d=json.loads(sys.argv[1]); a,b=sys.argv[2],sys.argv[3]
q=d["queue"]
ids=[r["id"] for r in q]
assert b not in ids, ("a closed report must not be in the queue", ids)
assert a in ids, ("a fixed report is not closed, so it is still in the queue", ids)
rank={"high":0,"medium":1,"low":2}
keys=[rank.get(r.get("severity"),3) for r in q]
assert keys==sorted(keys), ("most urgent first, no severity last", [(r.get("severity")) for r in q])
for r in q:
    assert set(("id","title","component","state")) <= set(r), r
PY

echo "triage: the workflow half: passed"
