#!/usr/bin/env bash
# moderation:queue — the verdict part
#
# PRECEDENCE, which is the only thing this part is for, and it is checked in both
# directions on the same run:
#
#   a rule that matched   -> the rule decides, whatever the model said
#   no rule matched       -> the model's label decides, exactly
#
# Neither direction alone proves anything. A part that always defers to the policy passes
# the first and fails the second; a part that ignores the policy does the reverse. And
# because nothing here compares the model's answer to an expected string, the two together
# hold whatever the model happens to say — which is the only way to assert precedence
# against a real model at all.
#
# One model call per reviewed item, and two items are reviewed: the linked one, where the
# fixture's deny rule fires, and the clean one, where nothing matches.
set -uo pipefail
# shellcheck source=components/moderation-domain/gate-lib.sh
. components/moderation-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

mod_requires_auth
gate_requires_capability "ai:inference/inference" \
  "the model is one interface away in this repository — not an HTTP client this part writes"
gate_requires_capability "policy:guard/guard" \
  "the rules are an engine in this repository — an \`if text.contains(\"://\")\` ladder is how this app becomes ungovernable, and it is what this check is looking for"
gate_requires_capability "event:bus/bus" \
  "a decision nothing downstream can see is a decision that did not leave the system"

gate_shim_config
trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"mod"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
post /test/rules '{}' >/dev/null || fail "the fixture could not write a policy rule"
IDS=$(post /test/seed '{}' | python3 -c "import sys,json;[print(i) for i in json.load(sys.stdin).get('item_ids',[])]")
LINKED=$(printf '%s' "$IDS" | sed -n 1p)
CLEAN=$(printf '%s' "$IDS" | sed -n 2p)
[ -n "$LINKED" ] && [ -n "$CLEAN" ] || fail "the fixture produced no items — the scaffold is broken, not the part"

review() { curl -s -X POST -H "$AUTH" "$B/api/items/$1/review"; }
review_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H "$AUTH" "$B/api/items/$1/review"; }

# --- the rule fires, and it wins ------------------------------------------------
D=$(review "$LINKED")
python3 - "$D" <<'PY' || fail "a matching deny rule did not decide the outcome"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("policy_rule") == "no-links", (
    "the fixture's rule matches this item (its text carries a link) and the decision does "
    f"not name it. A decision that cannot say what overruled what cannot be audited: {d}"
)
assert d.get("final") == "blocked", f"a deny rule that matched means blocked, whatever the model said: {d}"
assert d.get("model_said") in ("allow", "flag", "block"), \
    f"the decision must record the model's own label, from the three it was given: {d}"
conf = d.get("model_confidence")
assert isinstance(conf, int) and 0 <= conf <= 1000, \
    f"confidence is classify's 0..=1000 milli-units, passed through as-is: {conf!r}"
assert d.get("policy_reason"), f"the engine's reason belongs in the decision: {d}"
assert str(d.get("decided_at", "")).endswith("Z"), f"decided_at must be RFC3339 UTC: {d}"
PY
python3 - "$(get "/test/item/$LINKED")" <<'PY' || fail "the decision was answered but not stored"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("state") == "blocked", f"the item's state must become the decision's final: {d}"
assert (d.get("decision") or {}).get("policy_rule") == "no-links", f"the stored decision is incomplete: {d}"
PY

# --- nothing matches, and the model decides -------------------------------------
D=$(review "$CLEAN")
python3 - "$D" <<'PY' || fail "with no rule matching, the model's label must decide exactly"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert not d.get("policy_rule"), (
    "no rule in the fixture matches an item with no link, so policy_rule must be empty. "
    f"A part that reports a rule here is inventing one: {d}"
)
expected = {"allow": "allowed", "flag": "flagged", "block": "blocked"}
said = d.get("model_said")
assert said in expected, f"the model's label must be recorded: {d}"
assert d.get("final") == expected[said], (
    f"with the policy silent the model decides: it said {said!r}, so final must be "
    f"{expected[said]!r}, not {d.get('final')!r}"
)
PY

# --- reviewing twice is a conflict, not a second model call --------------------
AGAIN=$(review "$CLEAN")
CODE=$(review_code "$CLEAN")
[ "$CODE" = 409 ] || fail "reviewing an already-decided item must be 409, got $CODE"
python3 - "$AGAIN" <<'PY' || fail "a 409 must name the decision already on record"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("error") == "already_decided", d
assert d.get("final") in ("allowed", "flagged", "blocked"), f"the 409 must carry the stored final: {d}"
PY
CODE=$(review_code nope)
[ "$CODE" = 404 ] || fail "reviewing an unknown item must be 404, got $CODE"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$B/api/items/$CLEAN/review")
[ "$CODE" = 401 ] || fail "reviewing with no bearer must be 401, got $CODE"

# --- both decisions left the system --------------------------------------------
#
# Read off the bus through the router's fixture reader, because `/api/events` belongs to
# `queue` and is a stub while this part is judged. A decision only this component can see
# is not a decision anything downstream can act on.
python3 - "$(get /test/events)" "$LINKED" "$CLEAN" <<'PY' || fail "the decisions were not published to the bus"
import json, sys
events = json.loads(sys.argv[1] or "{}").get("events", [])
linked, clean = sys.argv[2], sys.argv[3]
published = {}
for e in events:
    payload = e.get("payload") or {}
    if isinstance(payload, dict) and payload.get("item"):
        published[payload["item"]] = payload.get("final")
for item, name in ((linked, "the blocked item"), (clean, "the item the model decided")):
    assert item in published, (
        name + " was decided but never published to moderation.decided. Everything "
        "downstream of this app learns about a decision from the bus. Published: "
        + repr(published)
    )
assert published[linked] == "blocked", (
    "the published outcome disagrees with the stored one: " + repr(published[linked])
)
PY

echo "moderation:queue — the verdict part: passed"
