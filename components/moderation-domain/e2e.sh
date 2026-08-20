#!/usr/bin/env bash
# moderation:queue — the whole API, all three parts at once
#
# The JOIN gate, and it uses no fixture at all: the rules are written through `queue`'s
# route, the item is submitted through `intake`'s, and the review goes through
# `verdict`'s. Each part passes alone against the ROUTER's fixtures — which write the
# contract's shapes — so a part that invented its own shape passes its own gate and fails
# here.
#
# What only this gate can see: a rule written by one part changing what another part
# decides, and the decision arriving on the bus for a third to read. One model call.
set -uo pipefail
# shellcheck source=components/moderation-domain/gate-lib.sh
. components/moderation-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

mod_requires_auth
gate_requires_capability "ratelimit:guard/limiter" "the composed API must still be counting submissions through the limiter"
gate_requires_capability "ai:inference/inference" "the composed API must still be asking the model through ai-inference"
gate_requires_capability "policy:guard/guard" "the composed API must still be deciding through the policy engine"
gate_requires_capability "event:bus/bus" "the composed API must still be publishing what it decided"

GATE_CONFIG="--config max-attempts=10 --config lockout-window=60"
gate_shim_config
trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"ada"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"
AUTH="authorization: Bearer $T"

# --- a rule, written through the part that owns rules --------------------------
#
# `author` rather than `has_link`: the fixture's rule is about links, so a part that
# somehow depends on the fixture rather than on what was just written fails here.
RULES='{"rules":[{"id":"deny-ada","action":"publish","effect":"deny","priority":1,"conditions":[{"left":"resource.author","op":"eq","right":"ada"}]}]}'
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" -d "$RULES" "$B/api/rules")
[ "$GOT" = 204 ] || fail "the queue part did not accept a rule set ($GOT), so precedence cannot be judged"

# --- an item, submitted through the part that owns submission ------------------
ID=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"text":"a perfectly ordinary message with nothing wrong with it"}' "$B/api/items" | field id)
[ -n "$ID" ] || fail "the intake part did not accept an item, so nothing else can be judged"

# --- reviewed through the part that owns review --------------------------------
#
# The content is benign, so a model left to itself would allow it. The rule denies it
# because of who wrote it. That gap is the join: three parts agreeing on the item's shape,
# the attribute names, and who has the last word.
D=$(curl -s -X POST -H "$AUTH" "$B/api/items/$ID/review")
python3 - "$D" <<'PY' || fail "the three parts do not agree — see which claim below failed"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert "error" not in d, (
    "the composed API refused a review of an item submitted through the real route. If "
    "this is not_found, `intake` (src/intake.rs) and `verdict` (src/verdict.rs) disagree "
    f"about the `items` collection or its shape. Got: {d}"
)
assert d.get("policy_rule") == "deny-ada", (
    "the rule written through /api/rules did not decide this review. If policy_rule is "
    "empty, `queue` (src/queue.rs) wrote rules the engine does not hold, or `verdict` "
    "(src/verdict.rs) passed target attributes under different names than the contract's "
    f"`author`/`has_link`/`model_label`. Got: {d}"
)
assert d.get("final") == "blocked", f"a deny rule that matched means blocked whatever the model said: {d}"
assert d.get("model_said") in ("allow", "flag", "block"), f"the model's own label must be recorded: {d}"
PY

# --- and the decision is readable through the part that owns the bus -----------
python3 - "$(curl -s -H "$AUTH" "$B/api/events")" "$ID" <<'PY' || fail "the decision never reached the part that reads the bus"
import json, sys
events = json.loads(sys.argv[1] or "{}").get("events") or []
item = sys.argv[2]
found = [e for e in events if isinstance(e.get("payload"), dict) and e["payload"].get("item") == item]
assert found, (
    "the review was decided but `queue`'s /api/events cannot see it. Either `verdict` "
    "published to a different topic than the contract's moderation.decided, or `queue` "
    f"polls a different one. Events seen: {events}"
)
assert found[0]["payload"].get("final") == "blocked", f"the published outcome disagrees with the decision: {found[0]}"
PY

# The queue reflects it too: blocked, and no longer pending.
python3 - "$(curl -s -H "$AUTH" "$B/api/queue?state=blocked")" "$ID" <<'PY' || fail "the queue does not reflect the decision the review made"
import json, sys
ids = [i.get("id") for i in (json.loads(sys.argv[1] or "{}").get("items") or [])]
assert sys.argv[2] in ids, f"a blocked item must appear under state=blocked: {ids}"
PY
python3 - "$(curl -s -H "$AUTH" "$B/api/queue")" "$ID" <<'PY' || fail "a decided item is still pending"
import json, sys
ids = [i.get("id") for i in (json.loads(sys.argv[1] or "{}").get("items") or [])]
assert sys.argv[2] not in ids, f"a decided item must leave the pending queue: {ids}"
PY

echo "moderation:queue — the whole API: passed"
