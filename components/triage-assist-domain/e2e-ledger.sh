#!/usr/bin/env bash
# triage:assist — the ledger part
#
# Judged without either other part existing, on purpose: the router notes every
# `/api/*` request it dispatches, so there is always traffic to show — and the two
# stubs answering 501 are traffic too. A gate that needed `intake` to be finished
# would report this part as broken every time it was judged first.
#
# `note` is the protocol here, not a helper: the router and both other parts call it,
# so its signature is fixed and only its body is this part's. The gate cannot see the
# function, so it asserts what the function must have caused — an event, with the
# contract's fields, retrievable by trace.
set -uo pipefail
# shellcheck source=components/triage-assist-domain/gate-lib.sh
. components/triage-assist-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

assist_requires_auth
gate_requires_capability "audit:log/recorder" \
  "a durable audit trail is a solved problem in this repository — writing events into the record store by hand is how this part fails"
gate_requires_capability "audit:log/query" \
  "reading the trail back is the other half of the same capability, and \`by-trace\` is what the trace filter is for"

trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
T=$(token ada)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"

# Two traces: one to write under, one to ask under. Asking under the trace being
# asked about would make the answer include the question, and the count race that
# follows would be nobody's fault.
TRACE=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1
OTHER=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2

for i in 1 2 3; do
  curl -s -o /dev/null -H "authorization: Bearer $T" \
    -H "traceparent: 00-$TRACE-0000000000000001-01" "$B/api/reports/r$i"
done

EVENTS=$(curl -s -H "authorization: Bearer $T" -H "traceparent: 00-$OTHER-0000000000000002-01" \
  "$B/api/audit?trace=$TRACE")
python3 - "$EVENTS" "$TRACE" <<'PY' || fail "GET /api/audit?trace= did not answer the three requests made under that trace"
import json, sys
d = json.loads(sys.argv[1] or "{}")
trace = sys.argv[2]
evs = d.get("events")
assert isinstance(evs, list), f"the answer has no events list: {d}"
assert len(evs) == 3, f"three requests were made under {trace}, the ledger has {len(evs)}: {evs}"
for e in evs:
    assert e.get("trace_id") == trace, f"an event from another trace came back: {e}"
    assert e.get("event") == "http.request", f"the router notes dispatched requests as http.request: {e}"
    assert e.get("subject") == "router", f"the router's own events are subject 'router': {e}"
    assert e.get("tenant") == "triage-assist", f"tenant must be the app's: {e}"
    assert e.get("id"), f"an event with no id cannot be referred to: {e}"
    assert isinstance(e.get("timestamp"), int) and e["timestamp"] > 0, f"timestamp must be unix seconds: {e}"
    assert e.get("detail"), f"an event with no detail says nothing an operator can use: {e}"
PY

# A trace nobody used is an empty list, not everything. `by-trace` returning the whole
# log looks like a working filter until the first real incident.
python3 - "$(curl -s -H "authorization: Bearer $T" "$B/api/audit?trace=$(printf 'c%.0s' $(seq 32))")" <<'PY' \
  || fail "a trace with no events must answer an empty list"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("events") == [], f"an unused trace answered {len(d.get('events', []))} events"
PY

# The limit, and the cap on it.
python3 - "$(curl -s -H "authorization: Bearer $T" "$B/api/audit?limit=2")" <<'PY' || fail "?limit=2 did not answer two events"
import json, sys
evs = json.loads(sys.argv[1] or "{}").get("events", [])
assert len(evs) == 2, f"?limit=2 answered {len(evs)}"
ts = [e["timestamp"] for e in evs]
assert ts == sorted(ts, reverse=True), f"newest first, and these are not: {ts}"
PY
python3 - "$(curl -s -H "authorization: Bearer $T" "$B/api/audit?limit=500")" <<'PY' || fail "?limit=500 was not capped"
import json, sys
evs = json.loads(sys.argv[1] or "{}").get("events", [])
assert len(evs) <= 100, f"the limit is capped at 100, this answered {len(evs)}"
PY
python3 - "$(curl -s -H "authorization: Bearer $T" "$B/api/audit")" <<'PY' || fail "the default limit is 20 and was not applied"
import json, sys
evs = json.loads(sys.argv[1] or "{}").get("events", [])
assert 0 < len(evs) <= 20, f"no limit given means 20, this answered {len(evs)}"
PY

# The trail is not public, and reading it is a read.
GOT=$(curl -s -o /dev/null -w '%{http_code}' "$B/api/audit")
[ "$GOT" = 401 ] || fail "reading the audit trail with no bearer must be 401, got $GOT"
WO=$(token nosy '["reports:write"]')
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $WO" "$B/api/audit")
[ "$GOT" = 403 ] || fail "a token that may write but not read must be 403 on the audit trail, got $GOT"

echo "triage:assist — the ledger part: passed"
