#!/usr/bin/env bash
# triage:assist — the intake part
#
# Judges three things that fail in three different ways.
#
# BEHAVIOUR, because compiling proves nothing: `cargo component build` passes on a
# crate that implements none of its world.
#
# COMPOSITION, because a hand-rolled email finder and a real PII scanner both answer
# 201 on a well-behaved body, and a hand-rolled counter and a real limiter both
# answer 429 eventually. The component's IMPORTS tell them apart.
#
# THE THREE REFUSALS, because collapsing 401, 403 and 429 into one status is the
# single most common way an intake looks finished and is not: a caller cannot tell
# "log in", "ask for access" and "wait" apart, and neither can an operator.
set -uo pipefail
# shellcheck source=components/triage-assist-domain/gate-lib.sh
. components/triage-assist-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

assist_requires_auth
gate_requires_capability "ratelimit:guard/limiter" \
  "counting attempts per subject is a solved problem in this repository — a hand-rolled counter in the store is how this part fails"
gate_requires_capability "pii:redact/redactor" \
  "masking PII is a solved problem in this repository and that capability is in the world for this part to USE, not to reimplement (see CONTRACT.md)"

# A limit low enough for a gate to trip in four requests. The default is 5 in 300s,
# which a test would have to make six calls to reach — and then the window makes the
# next test flaky. Three is the smallest number that still distinguishes "counts" from
# "refuses everything".
GATE_CONFIG="--config max-attempts=3 --config lockout-window=60"
trap gate_cleanup EXIT
gate_serve

token() { # token <subject> [scopes-json]
  post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token
}

# --- the three refusals, none of which spends an attempt -----------------------
REPORT='{"title":"Search returns nothing","body":"contact me at ada@example.test","component":"search"}'

expect_post 401 /api/reports "$REPORT" \
  "a report with no bearer token must be 401 unauthenticated"

RO=$(token reader '["reports:read"]')
[ -n "$RO" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $RO" -d "$REPORT" "$B/api/reports")
[ "$GOT" = 403 ] || fail "a token with only reports:read must be 403 forbidden, not $GOT — 401 says 'log in' to a caller who is logged in"

WRITER=$(token ada)
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $WRITER" -d '{"title":"","body":"x","component":"web"}' "$B/api/reports")
[ "$GOT" = 400 ] || fail "an empty title must be 400 invalid_report, got $GOT"

# --- a report goes in, and what is stored is masked ---------------------------
#
# The body carries an email, which is the point: the contract says what is STORED is
# masked, and that body goes on to a model. The raw address must not come back out.
RESP=$(curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $WRITER" \
  -d "$REPORT" "$B/api/reports")
ID=$(printf '%s' "$RESP" | field id)
[ -n "$ID" ] || fail "POST /api/reports returned no id: $RESP"

STORED=$(get "/test/report/$ID")
case "$STORED" in
  *ada@example.test*) fail "the reporter's email was stored verbatim — it must be masked: $STORED" ;;
esac
case "$STORED" in
  *'[EMAIL]'*) ;;
  *) fail "the body was not masked with pii:redact's placeholder: $STORED" ;;
esac

python3 - "$STORED" <<'PY' || fail "a new report must be open, and must record who reported it"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("state")=="open", d
assert d.get("reporter")=="ada", f"reporter must be the principal's subject, not {d.get('reporter')!r}"
assert d.get("component")=="search", d
assert "assist" not in d, "intake must not invent an assist — that is the assist part's job"
assert d.get("reported_at","").endswith("Z"), f"reported_at must be RFC3339 UTC: {d.get('reported_at')!r}"
PY

# --- reading it back, through the part's own route ---------------------------
READ=$(curl -s -H "authorization: Bearer $WRITER" "$B/api/reports/$ID")
python3 - "$READ" "$ID" <<'PY' || fail "GET /api/reports/{id} did not answer the stored report"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("title")=="Search returns nothing", d
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $WRITER" "$B/api/reports/nope")
[ "$GOT" = 404 ] || fail "an unknown report id must be 404, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' "$B/api/reports/$ID")
[ "$GOT" = 401 ] || fail "reading a report with no bearer must be 401, got $GOT"

# --- the filter, which is an index lookup and not a scan ----------------------
LIST=$(curl -s -H "authorization: Bearer $WRITER" "$B/api/reports?component=search")
python3 - "$LIST" "$ID" <<'PY' || fail "GET /api/reports?component= did not find the report just filed"
import json,sys
d=json.loads(sys.argv[1]); ids=[r.get("id") for r in d.get("reports",[])]
assert sys.argv[2] in ids, f"filtering on component=search missed it: {d}"
PY
LIST=$(curl -s -H "authorization: Bearer $WRITER" "$B/api/reports?component=nothing-files-bugs-here")
python3 - "$LIST" <<'PY' || fail "a filter matching nothing must answer an empty list, not everything"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("reports")==[], d
PY

# --- and the limit, which counts what was accepted ---------------------------
#
# A subject of its own: the key is the principal's subject, so counting `burst` cannot
# be disturbed by what `ada` did above — and if a part keys the limiter on something
# else (a path, a tenant, nothing at all), this is where that shows.
BURST=$(token burst)
for i in 1 2 3; do
  GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
    -H "authorization: Bearer $BURST" \
    -d "{\"title\":\"burst $i\",\"body\":\"b\",\"component\":\"web\"}" "$B/api/reports")
  [ "$GOT" = 201 ] || fail "report $i of 3 within the limit must be accepted, got $GOT"
done
LOCKED=$(curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $BURST" \
  -d '{"title":"burst 4","body":"b","component":"web"}' "$B/api/reports")
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $BURST" -d '{"title":"burst 5","body":"b","component":"web"}' "$B/api/reports")
[ "$GOT" = 429 ] || fail "the 4th report from one subject in the window must be 429 rate_limited, got $GOT (the limit is max-attempts=3)"
python3 - "$LOCKED" <<'PY' || fail "a 429 must tell the caller how long to wait"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("error")=="rate_limited", d
assert isinstance(d.get("retry_after"), int) and d["retry_after"] > 0, f"retry_after must be the seconds the limiter reported: {d}"
PY

# The other subject is unaffected — a limiter keyed on the wrong thing locks everyone
# out at once, and a gate that only ever used one subject would call that a pass.
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $WRITER" -d '{"title":"still fine","body":"b","component":"web"}' "$B/api/reports")
[ "$GOT" = 201 ] || fail "locking out one subject must not lock out another, got $GOT for a different subject"

echo "triage:assist — the intake part: passed"
