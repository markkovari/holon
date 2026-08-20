#!/usr/bin/env bash
# moderation:queue — the queue part
#
# No model call. What is checked instead is that the rules a reviewer READS are the rules
# a reviewer's decisions USED: they go into `policy:guard` and come back out of it. A part
# that keeps its own copy answers both routes correctly and drifts the moment anything
# else writes a rule — which the fixture does, on purpose, in the middle of this gate.
set -uo pipefail
# shellcheck source=components/moderation-domain/gate-lib.sh
. components/moderation-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

mod_requires_auth
gate_requires_capability "policy:guard/guard" \
  "the rules live in an engine in this repository — a copy of your own is how the rules a reviewer reads stop being the rules their decisions used"
gate_requires_capability "event:bus/bus" \
  "what has left the system is read off the bus, not out of a list this part kept"

trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"mod"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
IDS=$(post /test/seed '{}' | python3 -c "import sys,json;[print(i) for i in json.load(sys.stdin).get('item_ids',[])]")
FIRST=$(printf '%s' "$IDS" | sed -n 1p)
[ -n "$FIRST" ] || fail "the fixture produced no items — the scaffold is broken, not the part"

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' "$B/api/queue")
[ "$GOT" = 401 ] || fail "reading the queue with no bearer must be 401, got $GOT"
RO=$(post /test/token '{"subject":"reader","scopes":["items:read"]}' | field token)
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $RO" -d '{"rules":[]}' "$B/api/rules")
[ "$GOT" = 403 ] || fail "writing rules needs items:moderate — a read-only token must be 403, got $GOT"

# --- what is waiting ------------------------------------------------------------
python3 - "$(curl -s -H "$AUTH" "$B/api/queue")" "$IDS" <<'PY' || fail "GET /api/queue did not answer what is pending"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
items = d.get("items")
assert isinstance(items, list) and len(items) >= 2, f"two items were seeded and the queue shows {d}"
ids = [i.get("id") for i in items]
seeded = [x for x in sys.argv[2].split("\n") if x]
for s in seeded:
    assert s in ids, f"a pending item is missing from the queue: {s} not in {ids}"
for i in items:
    assert i.get("state") == "pending", f"the default queue is the pending one: {i}"
    assert i.get("id"), f"an item without its id cannot be reviewed: {i}"
stamps = [i.get("submitted_at") for i in items]
assert stamps == sorted(stamps), f"a queue is oldest first, not newest: {stamps}"
PY
python3 - "$(curl -s -H "$AUTH" "$B/api/queue?state=blocked")" <<'PY' || fail "filtering the queue by a state nothing is in must be empty"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("items") == [], "nothing is blocked yet and the queue said otherwise"
PY

# --- the rules go in through the engine, and come back out of it ---------------
RULES='{"rules":[{"id":"deny-shouting","action":"publish","effect":"deny","priority":5,"conditions":[{"left":"resource.model_label","op":"eq","right":"block"}]}]}'
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" -d "$RULES" "$B/api/rules")
[ "$GOT" = 204 ] || fail "writing a valid rule set must be 204, got $GOT"
python3 - "$(curl -s -H "$AUTH" "$B/api/rules")" <<'PY' || fail "the rules did not come back as they were written"
import json, sys
rules = json.loads(sys.argv[1] or "{}").get("rules")
assert isinstance(rules, list) and len(rules) == 1, f"one rule was written and {rules} came back"
r = rules[0]
assert r.get("id") == "deny-shouting", r
assert r.get("effect") == "deny", r
assert r.get("priority") == 5, r
conds = r.get("conditions")
assert conds and conds[0].get("left") == "resource.model_label" and conds[0].get("right") == "block", r
PY

# The fixture writes a DIFFERENT rule set straight through the engine. A part answering
# from its own copy still says `deny-shouting`; a part reading the engine says `no-links`.
post /test/rules '{}' >/dev/null || fail "the fixture could not write a rule"
python3 - "$(curl -s -H "$AUTH" "$B/api/rules")" <<'PY' || fail "GET /api/rules answered a stale copy instead of the engine"
import json, sys
rules = json.loads(sys.argv[1] or "{}").get("rules") or []
ids = [r.get("id") for r in rules]
assert ids == ["no-links"], (
    "something else replaced the rules through policy:guard and this route still reports "
    f"{ids}. The rules a reviewer reads must be the rules the engine holds, or they are "
    "not the rules any decision used."
)
PY

# An invalid rule is refused here, because a rule the engine rejects later is a rule
# nobody wrote down.
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"rules":[{"id":"x","action":"publish","effect":"maybe","priority":1,"conditions":[]}]}' "$B/api/rules")
[ "$GOT" = 400 ] || fail "an unknown effect must be 400 invalid_rule, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"rules":[{"id":"x","action":"publish","effect":"deny","priority":1,"conditions":[{"left":"a","op":"sideways","right":"b"}]}]}' "$B/api/rules")
[ "$GOT" = 400 ] || fail "an unknown op must be 400 invalid_rule, got $GOT"

# --- what has left the system --------------------------------------------------
#
# `verdict` is a stub here, so nothing has published a decision. The bus is empty and this
# route must say so — the check is that it READS the bus rather than inventing a list, and
# that reading twice gives the same answer, which an `ack` would not.
python3 - "$(curl -s -H "$AUTH" "$B/api/events")" <<'PY' || fail "GET /api/events did not answer an empty bus cleanly"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("events") == [], f"nothing has been published and this route answered {d}"
PY
ONE=$(curl -s -H "$AUTH" "$B/api/events")
TWO=$(curl -s -H "$AUTH" "$B/api/events")
[ "$ONE" = "$TWO" ] || fail "reading the events twice gave different answers — a read that consumes is not a read (do not ack)"

echo "moderation:queue — the queue part: passed"
