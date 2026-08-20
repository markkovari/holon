#!/usr/bin/env bash
# treasury:ledger — the whole API, all three parts at once
#
# The JOIN, and it is the only gate that can prove the thing this app is for: after a storm of
# concurrent credits and a storm of concurrent transfers, an INDEPENDENT recomputation from the
# journal agrees with every stored balance to the minor unit.
#
# Nothing here is a fixture. Accounts are opened through `accounts`, money is moved through
# `transfers`, and the books are checked by `reconcile` — which was written by an agent that
# never saw either of the other two.
set -uo pipefail
# shellcheck source=components/treasury-ledger-domain/gate-lib.sh
. components/treasury-ledger-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

treasury_requires_auth
gate_requires_capability "records:store/store" "the composed API must still be serialising its debits on the store's revision"
gate_requires_capability "money:amount/arithmetic" "the composed API must still do its arithmetic in money:amount"
gate_requires_capability "ledger:doubleentry/ledger" "the composed API must still balance its journal through the ledger"
gate_requires_capability "idempotency:guard/store" "the composed API must still be exactly-once"
gate_requires_capability "fsm:workflow/engine" "the composed API must still drive the transfer lifecycle"

trap gate_cleanup EXIT
gate_serve

T=$(post /test/token '{"subject":"treasurer"}' | field token)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the parts"
AUTH="authorization: Bearer $T"
acct() { curl -s -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d "{\"name\":\"$1\",\"currency\":\"EUR\",\"start\":\"0.00\"}" "$B/api/accounts" | field id; }

A=$(acct join-a)
Z=$(acct join-b)
[ -n "$A" ] && [ -n "$Z" ] || fail "the accounts part did not open an account, so nothing else can be judged"

# --- fund one side with twenty concurrent credits ------------------------------
storm 20 -X POST -H 'content-type: application/json' -H "$AUTH" -d '{"amount":"5.00"}' \
  "$B/api/accounts/$A/credit" >/dev/null
FUNDED=$(units_of "$A")
[ "$FUNDED" = 10000 ] || fail "twenty concurrent credits of 5.00 should fund the account with 10000 minor units, it holds $FUNDED"

# --- move it across in ten concurrent transfers -------------------------------
#
# Ten transfers of 10.00 from a balance of 100.00: all ten fit, and all ten must land, which is
# the mirror image of the transfers gate's storm — there the answer was "exactly one", here it
# is "all of them", and only a correct implementation gives both.
CODES=$(seq 10 | xargs -P 10 -I{} curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  -H 'content-type: application/json' -H "$AUTH" -H "idempotency-key: join-{}" \
  -d "{\"from\":\"$A\",\"to\":\"$Z\",\"amount\":\"10.00\"}" "$B/api/transfers" | sort | uniq -c | tr -s ' ' | paste -sd' ' -)
python3 - "$(units_of "$A")" "$(units_of "$Z")" "$CODES" <<'PY' || fail "ten concurrent transfers that all fit did not all land"
import sys
a, z, codes = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
assert a + z == 10000, f"the pair held 10000 and now holds {a + z}. Codes: [{codes}]"
assert a == 0 and z == 10000, (
    f"ten transfers of 10.00 out of 100.00 all fit, so the source should be empty and the "
    f"destination full: from={a} to={z}. Codes: [{codes}]"
)
PY

# --- and the auditor agrees ---------------------------------------------------
#
# `reconcile` recomputes both balances from the journal alone. It was written against the same
# contract by an agent that never saw `transfers`, and if the two disagree about the journal's
# shape — a field name, a direction, a unit — this is where it shows.
REPORT=$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -H "idempotency-key: join-audit" \
  -d "{\"opened\":[{\"account\":\"$A\",\"units\":10000},{\"account\":\"$Z\",\"units\":0}]}" \
  "$B/api/reconcile")
python3 - "$REPORT" <<'PY' || fail "the auditor disagrees with the mover — see which claim below failed"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the reconcile route answered an empty body"
d = json.loads(raw)
assert "error" not in d, (
    "the reconcile part refused a report over accounts moved through the real routes. If this "
    f"is a store or shape error, the two parts disagree about the `journal` collection: {d}"
)
assert d.get("journal_lines") == 10, (
    f"ten transfers settled and the journal has {d.get('journal_lines')} lines. `transfers` "
    "writes them and `reconcile` reads them, and nothing else in the app would notice if they "
    "disagreed about the collection or the field names."
)
assert d.get("balanced") is True, (
    "the recomputation from the journal disagrees with the stored balances. Every unit that "
    f"moved was journalled, or it was not: {d}"
)
assert d.get("drift") == [], f"drift must be empty when the books agree: {d}"
PY

echo "treasury:ledger — the whole API: passed"
