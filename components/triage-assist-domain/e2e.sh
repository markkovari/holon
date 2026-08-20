#!/usr/bin/env bash
# triage:assist — the whole API, all three parts at once
#
# The JOIN gate. Each part's own gate judges it with the other two stubbed, which is
# what makes three agents possible — and is exactly why something has to judge them
# together afterwards. Three parts that each pass alone can still disagree, and the
# disagreement is invisible in any single part's gate.
#
# The audit trail is what makes the chain checkable. `ledger` cannot show
# `reports.create ok` unless `intake` called `note` with the contract's event name and
# the principal's subject; `assist` cannot answer at all unless `intake` stored a
# document in the shape the contract describes. So one request through the whole API,
# correlated by one trace, asserts all three parts agreed — and a part that invented
# its own storage shape or its own event names fails HERE having passed its own gate.
#
# One model call, like the assist part's own gate: the cost of this gate is what the
# loop pays per attempt, and two would double it for nothing.
set -uo pipefail
# shellcheck source=components/triage-assist-domain/gate-lib.sh
. components/triage-assist-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

# Every capability the app is composed of, asserted on the joined artifact. A part
# that hand-rolled one and passed its own gate on behaviour alone stops here.
assist_requires_auth
gate_requires_capability "ratelimit:guard/limiter" \
  "the composed API must still be counting attempts through the limiter component"
gate_requires_capability "pii:redact/redactor" \
  "the composed API must still be masking through pii-redact"
gate_requires_capability "ai:inference/inference" \
  "the composed API must still be reaching the model through ai-inference"
gate_requires_capability "audit:log/recorder" \
  "the composed API must still be recording through audit-log"

# Three accepted reports before the lockout, and the third one is this gate's own
# report — so the limit must be at least 4 for the happy path to survive it.
GATE_CONFIG="--config max-attempts=4 --config lockout-window=60"
gate_shim_config
trap gate_cleanup EXIT
gate_serve

TRACE=deadbeefdeadbeefdeadbeefdeadbee1
TP="traceparent: 00-$TRACE-0000000000000001-01"

T=$(post /test/token '{"subject":"ada"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"

# --- a report goes all the way through ---------------------------------------
RESP=$(curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $T" -H "$TP" \
  -d '{"title":"Checkout total is wrong","body":"off by a cent, mail me at ada@example.test","component":"billing"}' \
  "$B/api/reports")
ID=$(printf '%s' "$RESP" | field id)
[ -n "$ID" ] || fail "POST /api/reports returned no id: $RESP"

READ=$(curl -s -H "authorization: Bearer $T" -H "$TP" "$B/api/reports/$ID")
case "$READ" in
  *ada@example.test*) fail "the composed API stored the reporter's email verbatim: $READ" ;;
esac

ASSIST=$(curl -s -X POST -H "authorization: Bearer $T" -H "$TP" "$B/api/reports/$ID/assist")
python3 - "$ASSIST" <<'PY' || fail "the assist part could not act on what the intake part stored"
import json, sys
a = json.loads(sys.argv[1] or "{}")
assert a.get("severity") in ("critical", "major", "minor"), f"no usable severity: {a}"
assert len((a.get("summary") or "").strip()) >= 20, f"no usable summary: {a}"
PY

# The whole point of the join: the trail proves what the other two did.
EVENTS=$(curl -s -H "authorization: Bearer $T" "$B/api/audit?trace=$TRACE")
python3 - "$EVENTS" <<'PY' || fail "the ledger cannot show what the other two parts did — the three parts disagree"
import json, sys
evs = json.loads(sys.argv[1] or "{}").get("events", [])
pairs = {(e.get("event"), e.get("outcome")) for e in evs}
assert ("reports.create", "ok") in pairs, \
    f"no reports.create/ok in the trail: the intake part did not note an accepted report under the contract's name. Got {sorted(pairs)}"
assert ("reports.assist", "ok") in pairs, \
    f"no reports.assist/ok in the trail: the assist part did not note the model's answer. Got {sorted(pairs)}"
subjects = {e.get("subject") for e in evs if e.get("event") != "http.request"}
assert subjects == {"ada"}, \
    f"the parts' own events must carry the principal's subject, not {subjects} — an audit trail that cannot say WHO is not one"
PY

# --- and the limit is still a limit once everything is wired together ---------
for i in 1 2 3; do
  curl -s -o /dev/null -X POST -H 'content-type: application/json' -H "authorization: Bearer $T" -H "$TP" \
    -d "{\"title\":\"noise $i\",\"body\":\"b\",\"component\":\"web\"}" "$B/api/reports"
done
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $T" -H "$TP" -d '{"title":"one too many","body":"b","component":"web"}' "$B/api/reports")
[ "$GOT" = 429 ] || fail "past the limit the composed API must answer 429, got $GOT"

python3 - "$(curl -s -H "authorization: Bearer $T" "$B/api/audit?trace=$TRACE")" <<'PY' \
  || fail "a throttled report left no trace — an operator cannot tell a rate limit from an outage"
import json, sys
evs = json.loads(sys.argv[1] or "{}").get("events", [])
assert ("reports.create", "throttled") in {(e.get("event"), e.get("outcome")) for e in evs}, \
    "the intake part refused for the rate limit and noted nothing under the contract's outcome name"
PY

echo "triage:assist — the whole API: passed"
