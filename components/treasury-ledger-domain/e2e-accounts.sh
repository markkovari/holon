#!/usr/bin/env bash
# treasury:ledger — the accounts part
#
# One property matters here and it cannot be tested one request at a time: twenty-four
# concurrent credits must all land. `records:store` is optimistic, so two requests that read
# the same revision and both write will see one refused — and the obvious implementation reports
# that refusal to the caller as a 409 and drops the money. Measured on this host with the naive
# version: 23.00 and 21.00 out of 24 credits of 1.00. Nothing errors. The balance is just wrong.
set -uo pipefail
# shellcheck source=components/treasury-ledger-domain/gate-lib.sh
. components/treasury-ledger-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

treasury_requires_auth
gate_requires_capability "money:amount/arithmetic" \
  "every amount in this app comes from money:amount — hand-parsed decimals and hand-added cents are the wrong work"
gate_requires_capability "records:store/store" \
  "the store is where accounts live, and its revision is the only thing standing between two concurrent credits"

trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
W=$(token treasurer)
[ -n "$W" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
AUTH="authorization: Bearer $W"
open_acct() { curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d "$1" "$B/api/accounts"; }
open_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" -d "$1" "$B/api/accounts"; }

# --- the refusals ---------------------------------------------------------------
expect_post 401 /api/accounts '{"name":"x","currency":"EUR"}' "opening an account with no bearer must be 401"
RO=$(token reader '["accounts:read"]')
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $RO" -d '{"name":"x","currency":"EUR"}' "$B/api/accounts")
[ "$GOT" = 403 ] || fail "a read-only token must be 403 on opening an account, got $GOT"
GOT=$(open_code '{"name":"","currency":"EUR"}')
[ "$GOT" = 400 ] || fail "an empty name must be 400 invalid_account, got $GOT"
GOT=$(open_code '{"name":"x","currency":"QQQ"}')
[ "$GOT" = 400 ] || fail "a currency money:amount does not know must be 400 bad_money, got $GOT"
GOT=$(open_code '{"name":"x","currency":"EUR","start":"1"}')
[ "$GOT" = 400 ] || fail "\"1\" is not a EUR amount (parse wants both decimals) — must be 400 bad_money, got $GOT"

# --- one account, one credit ----------------------------------------------------
ID=$(open_acct '{"name":"ledger-test","currency":"EUR","start":"10.00"}' | field id)
[ -n "$ID" ] || fail "POST /api/accounts returned no id"
[ "$(units_of "$ID")" = 1000 ] || fail "an account opened at 10.00 must store 1000 minor units, got $(units_of "$ID")"

GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"amount":"0.00"}' "$B/api/accounts/$ID/credit")
[ "$GOT" = 400 ] || fail "a zero credit must be 400 invalid_amount, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"amount":"-5.00"}' "$B/api/accounts/$ID/credit")
[ "$GOT" = 400 ] || fail "a negative credit must be 400 invalid_amount — that is a transfer with one side missing, got $GOT"
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "$AUTH" \
  -d '{"amount":"1.00"}' "$B/api/accounts/nope/credit")
[ "$GOT" = 404 ] || fail "crediting an unknown account must be 404, got $GOT"

python3 - "$(curl -s -X POST -H 'content-type: application/json' -H "$AUTH" -d '{"amount":"2.50"}' "$B/api/accounts/$ID/credit")" <<'PY' || fail "a single credit did not answer the new balance"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the credit route answered an empty body"
d = json.loads(raw)
assert d.get("units") == 1250, f"10.00 + 2.50 is 1250 minor units: {d}"
PY

# --- and now all at once --------------------------------------------------------
#
# A fresh account at zero, twenty-four credits of 1.00 fired in parallel. Every one of them
# must be reflected in the balance: 2400. A part that surfaces a revision conflict instead of
# re-reading ends short, and by a different amount every run.
STORM=$(open_acct '{"name":"storm","currency":"EUR","start":"0.00"}' | field id)
[ -n "$STORM" ] || fail "could not open the account for the contention test"
CODES=$(storm 24 -X POST -H 'content-type: application/json' -H "$AUTH" -d '{"amount":"1.00"}' \
  "$B/api/accounts/$STORM/credit" | sort | uniq -c | tr -s ' ' | paste -sd' ' -)
AFTER=$(units_of "$STORM")
python3 - "$AFTER" "$CODES" <<'PY' || fail "concurrent credits did not all land — this is the property this part exists for"
import sys
after, codes = sys.argv[1], sys.argv[2]
assert after.isdigit(), f"the account's balance could not be read back: {after!r}"
after = int(after)
assert "409" not in codes, (
    f"a credit was answered 409 [{codes}]. A revision conflict is the store saying 'read "
    "again', not a refusal to show the caller: nobody asked whether the account had changed, "
    "they asked to add money to it."
)
assert after == 2400, (
    f"twenty-four concurrent credits of 100 minor units left the account at {after}, not 2400 "
    f"— {(2400 - after) // 100} of them vanished. Status codes: [{codes}]. Read the conflict "
    "and retry from what is there now."
)
PY

# Not a fluke of one run, and not a fluke of an empty account: again, on top of a balance.
CODES=$(storm 24 -X POST -H 'content-type: application/json' -H "$AUTH" -d '{"amount":"0.25"}' \
  "$B/api/accounts/$STORM/credit" | sort | uniq -c | tr -s ' ' | paste -sd' ' -)
AFTER=$(units_of "$STORM")
python3 - "$AFTER" "$CODES" <<'PY' || fail "a second storm lost money, so the first one passing was luck"
import sys
after, codes = sys.argv[1], sys.argv[2]
assert int(after) == 3000, (
    f"2400 plus twenty-four credits of 25 is 3000, and the account is at {after}. Codes: [{codes}]"
)
PY

echo "treasury:ledger — the accounts part: passed"
