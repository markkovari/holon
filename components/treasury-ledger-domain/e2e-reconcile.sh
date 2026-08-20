#!/usr/bin/env bash
# treasury:ledger — the reconcile part
#
# The auditor, and the gate's job is to make sure it actually audits. A part that answers
# `balanced: true` without reading the journal passes any test where the books happen to be
# right — so this one seeds a journal that DISAGREES with the balances on purpose and requires
# the exact delta, in the right direction, on the right account.
set -uo pipefail
# shellcheck source=components/treasury-ledger-domain/gate-lib.sh
. components/treasury-ledger-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

treasury_requires_auth
gate_requires_capability "money:amount/arithmetic" \
  "a reconciliation that adds up cents by hand is a reconciliation nobody should believe"
gate_requires_capability "idempotency:guard/store" \
  "a report run twice must not produce two different truths"

trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"auditor"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $T"
IDS=$(post /test/seed '{"start":"50.00"}' | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw: sys.exit('the fixture answered nothing')
[print(i) for i in json.loads(raw).get('account_ids', [])]
")
L=$(printf '%s' "$IDS" | sed -n 1p)
R=$(printf '%s' "$IDS" | sed -n 2p)
[ -n "$L" ] && [ -n "$R" ] || fail "the fixture produced no accounts — the scaffold is broken, not the part"
line() { post /test/journal "{\"from\":\"$1\",\"to\":\"$2\",\"units\":$3}" >/dev/null; }
run() { # run <key> <opened-json>
  curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -H "idempotency-key: $1" \
    -d "{\"opened\":$2}" "$B/api/reconcile"
}
OPENED="[{\"account\":\"$L\",\"units\":5000},{\"account\":\"$R\",\"units\":5000}]"

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "idempotency-key: k" -d "{\"opened\":$OPENED}" "$B/api/reconcile")
[ "$GOT" = 401 ] || fail "reconciling with no bearer must be 401, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d "{\"opened\":$OPENED}" "$B/api/reconcile")
[ "$GOT" = 400 ] || fail "a reconciliation with no Idempotency-Key must be 400, got $GOT"

# --- an empty journal agrees with untouched balances ----------------------------
python3 - "$(run empty "$OPENED")" <<'PY' || fail "with no journal and untouched balances the books must balance"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the reconcile route answered an empty body"
d = json.loads(raw)
assert d.get("checked") == 2, f"two accounts were given and {d.get('checked')} were checked: {d}"
assert d.get("balanced") is True, f"nothing has moved, so the books balance: {d}"
assert d.get("drift") == [], f"drift must be empty when nothing disagrees: {d}"
assert d.get("journal_lines") == 0, f"the journal is empty and this reports {d.get('journal_lines')}"
PY

# --- a journal that disagrees with the balances --------------------------------
#
# Two lines moving 10.00 from left to right, and NOBODY touched the balances. A reconciliation
# that reads the journal finds left 20.00 short of what the journal says and right 20.00 over.
# One that trusts the balances reports `balanced: true` and is useless.
line "$L" "$R" 1000
line "$L" "$R" 1000
python3 - "$(run drifted "$OPENED")" "$L" "$R" <<'PY' || fail "the reconciliation did not find the drift the journal describes"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the reconcile route answered an empty body"
d = json.loads(raw)
left, right = sys.argv[2], sys.argv[3]
assert d.get("journal_lines") == 2, f"two journal lines were written and this read {d.get('journal_lines')}: {d}"
assert d.get("balanced") is False, (
    "the journal says 20.00 moved and neither balance changed, and this reports the books as "
    f"balanced. A reconciliation that does not read the journal is worse than none: {d}"
)
drift = {x["account"]: x for x in d.get("drift") or []}
assert set(drift) == {left, right}, f"both accounts drifted and the report names {list(drift)}: {d}"
# left: opened 5000, journal says -2000, so expected 3000; stored is still 5000 -> +2000 actual.
assert drift[left]["expected"] == 3000, f"left expected 5000-2000=3000: {drift[left]}"
assert drift[left]["actual"] == 5000, f"left's stored balance is 5000: {drift[left]}"
assert drift[left]["delta"] == 2000, f"left holds 2000 more than the journal justifies: {drift[left]}"
assert drift[right]["expected"] == 7000, f"right expected 5000+2000=7000: {drift[right]}"
assert drift[right]["delta"] == -2000, f"right holds 2000 less than the journal justifies: {drift[right]}"
PY

# --- the same report twice is the same report ----------------------------------
ONE=$(run twice "$OPENED")
TWO=$(run twice "$OPENED")
python3 - "$ONE" "$TWO" <<'PY' || fail "the same idempotency key produced two different reports"
import json, sys
a, b = json.loads(sys.argv[1] or "{}"), json.loads(sys.argv[2] or "{}")
assert a == b, f"a report is a report: running it again under the same key must answer the same thing.\n  {a}\n  {b}"
PY

# A different key sees the world as it is now — one more line, one more finding.
line "$L" "$R" 500
python3 - "$(run fresh "$OPENED")" <<'PY' || fail "a new reconciliation did not see the newest journal line"
import json, sys
d = json.loads((sys.argv[1] or "").strip() or "{}")
assert d.get("journal_lines") == 3, f"three lines exist now and this read {d.get('journal_lines')}: {d}"
PY

# --- the journal read route ----------------------------------------------------
python3 - "$(curl -s -H "$AUTH" "$B/api/journal?limit=2")" <<'PY' || fail "GET /api/journal did not honour its limit or its order"
import json, sys
lines = json.loads((sys.argv[1] or "").strip() or "{}").get("lines")
assert isinstance(lines, list) and len(lines) == 2, f"?limit=2 answered {lines}"
ats = [l.get("at") for l in lines]
assert ats == sorted(ats), f"oldest first, and these are not: {ats}"
PY

echo "treasury:ledger — the reconcile part: passed"
