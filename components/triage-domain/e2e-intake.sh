#!/usr/bin/env bash
# triage: the intake half
#
# Judges the `intake` part on two different things.
#
# BEHAVIOUR, because compiling proves nothing: `cargo component build` passes on a
# crate that implements none of its world. So this starts the component and asks it
# for things.
#
# COMPOSITION, because a hand-rolled `@`-finder and a real PII scanner both answer
# 201 on a well-behaved body. The component's IMPORTS tell them apart — and so does
# one report body containing an email, but only the import check also catches a part
# that reimplemented the masking correctly and still should not have.
set -uo pipefail
# shellcheck source=components/triage-domain/gate-lib.sh
. components/triage-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

gate_requires_capability "pii:redact/redactor" \
  "masking PII is a solved problem in this repository and that capability is in the world for this part to USE, not to reimplement (see CONTRACT.md)"

trap gate_cleanup EXIT
gate_serve

# --- a report goes in ---------------------------------------------------------
#
# The body carries an email, which is the point: the contract says what is STORED is
# masked, so the raw address must not come back out.
BODY='{"title":"Search returns nothing","body":"contact me at ada@example.test","component":"search"}'
RESP=$(post /api/reports "$BODY")
ID=$(printf '%s' "$RESP" | field id)
[ -n "$ID" ] || fail "POST /api/reports returned no id: $RESP"

STORED=$(get "/api/reports/$ID")
case "$STORED" in
  *ada@example.test*) fail "the reporter's email was stored verbatim — it must be masked: $STORED" ;;
esac
case "$STORED" in
  *'[EMAIL]'*) ;;
  *) fail "the body was not masked with pii:redact's placeholder: $STORED" ;;
esac

# state open, and no severity until it is triaged
python3 - "$STORED" <<'PY' || fail "a new report must be open with no severity"
import json,sys
d=json.loads(sys.argv[1])
assert d.get("state")=="open", d
assert "severity" not in d or d["severity"] in (None,""), d
PY

# --- what a bad request is ----------------------------------------------------
expect_post 400 /api/reports '{"title":"","body":"b","component":"c"}' "an empty title is a 400"
expect_post 400 /api/reports '{"title":"t","component":"c"}' "a missing body is a 400"
expect_post 400 /api/reports '{"title":"t","body":"b"}' "a missing component is a 400"
expect_post 400 /api/reports 'not json at all' "malformed JSON is a 400"
expect_get 404 "/api/reports/nope" "an unknown report is a 404"

# --- the duplicate rule -------------------------------------------------------
#
# Same component AND same title, and the existing one is not closed.
expect_post 409 /api/reports "$BODY" "the same title in the same component is a duplicate"
DUP=$(post /api/reports "$BODY")
EXISTING=$(printf '%s' "$DUP" | field existing)
[ "$EXISTING" = "$ID" ] || fail "a duplicate must name the report it collides with (got '$EXISTING', wanted '$ID')"
# A different component is not a duplicate.
expect_post 201 /api/reports '{"title":"Search returns nothing","body":"b","component":"billing"}' \
  "the same title in a DIFFERENT component is not a duplicate"

# --- listing and filtering ----------------------------------------------------
gate_seed >/dev/null
python3 - "$(get '/api/reports')" <<'PY' || fail "GET /api/reports must list every report"
import json,sys
d=json.loads(sys.argv[1])
assert isinstance(d.get("reports"),list) and len(d["reports"])>=4, d
PY
python3 - "$(get '/api/reports?component=search')" <<'PY' || fail "?component= must filter"
import json,sys
d=json.loads(sys.argv[1])
rs=d["reports"]
assert rs and all(r.get("component")=="search" for r in rs), d
PY
python3 - "$(get '/api/reports?state=open&component=billing')" <<'PY' || fail "?state= and ?component= must AND"
import json,sys
d=json.loads(sys.argv[1])
rs=d["reports"]
assert rs and all(r.get("state")=="open" and r.get("component")=="billing" for r in rs), d
PY

echo "triage: the intake half: passed"
