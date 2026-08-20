#!/usr/bin/env bash
# moderation:queue — the intake part
set -uo pipefail
# shellcheck source=components/moderation-domain/gate-lib.sh
. components/moderation-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

mod_requires_auth
gate_requires_capability "ratelimit:guard/limiter" \
  "counting submissions per subject is a solved problem in this repository — a counter in the record store is how this part fails"

# Three accepted submissions, then the lockout. Low enough for a gate to reach in four
# requests, high enough to tell "counts" from "refuses everything".
GATE_CONFIG="--config max-attempts=3 --config lockout-window=60"
trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
W=$(token ada)
[ -n "$W" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
ITEM='{"text":"has anyone tried the new deploy flow?"}'
submit() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/items"; }
submit_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$2" "$B/api/items"; }

# --- the refusals, none of which spends an attempt -----------------------------
expect_post 401 /api/items "$ITEM" "submitting with no bearer must be 401"
RO=$(token reader '["items:read"]')
GOT=$(submit_code "$RO" "$ITEM")
[ "$GOT" = 403 ] || fail "a token with only items:read must be 403 on a submission, not $GOT"
GOT=$(submit_code "$W" '{"text":""}')
[ "$GOT" = 400 ] || fail "empty text must be 400 invalid_item, got $GOT"

# --- an item goes in ------------------------------------------------------------
ID=$(submit "$W" "$ITEM" | field id)
[ -n "$ID" ] || fail "POST /api/items returned no id"
python3 - "$(get "/test/item/$ID")" <<'PY' || fail "the stored item is not what the contract describes"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("state") == "pending", f"a new item is pending: {d}"
assert d.get("author") == "ada", f"author must be the principal's subject, not {d.get('author')!r}"
assert "decision" not in d, "intake must not invent a decision — that is the verdict part's job"
assert str(d.get("submitted_at", "")).endswith("Z"), f"submitted_at must be RFC3339 UTC: {d.get('submitted_at')!r}"
PY
READ=$(curl -s -H "authorization: Bearer $W" "$B/api/items/$ID")
python3 - "$READ" <<'PY' || fail "GET /api/items/{id} did not answer the stored item"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("text") == "has anyone tried the new deploy flow?", d
PY
GOT=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $W" "$B/api/items/nope")
[ "$GOT" = 404 ] || fail "an unknown item id must be 404, got $GOT"

# --- and the limit, which counts what was accepted -----------------------------
BURST=$(token burst)
for i in 1 2 3; do
  GOT=$(submit_code "$BURST" "{\"text\":\"burst $i\"}")
  [ "$GOT" = 201 ] || fail "submission $i of 3 within the limit must be accepted, got $GOT"
done
LOCKED=$(submit "$BURST" '{"text":"burst 4"}')
python3 - "$LOCKED" <<'PY' || fail "past the limit the part must refuse and say how long to wait"
import json, sys
d = json.loads(sys.argv[1] or "{}")
assert d.get("error") == "rate_limited", d
assert isinstance(d.get("retry_after"), int) and d["retry_after"] > 0, f"retry_after must be the limiter's seconds: {d}"
PY
GOT=$(submit_code "$W" '{"text":"still fine"}')
[ "$GOT" = 201 ] || fail "locking out one subject must not lock out another, got $GOT"

echo "moderation:queue — the intake part: passed"
