# treasury:ledger's gates, named. Everything they do lives in `components/gate-lib.sh`.
GATE_CRATE=treasury-ledger-domain
GATE_APP=treasury
GATE_PKGS="-p treasury-ledger-domain -p auth-guard -p rate-limiter -p audit-log \
-p money -p ledger -p idempotency-guard -p fsm-workflow -p outbox \
-p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

treasury_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}

# --- the whole point of this app: many requests at once ----------------------------
#
# `xargs -P` rather than a loop: a loop is a sequence, and a sequence proves nothing here.
# Every naive implementation in this app passes when requests arrive one at a time.
storm() { # storm <parallel> <method-args...> — prints one status code per line
  local n="$1"; shift
  seq "$n" | xargs -P "$n" -I{} curl -s -o /dev/null -w '%{http_code}\n' "$@"
}

# The balance a part actually stored, read through the router's fixture so no part's own read
# route can flatter it.
units_of() { # units_of <account id>
  get "/test/account/$1" | python3 -c "
import json, sys
raw = sys.stdin.read().strip()
if not raw:
    sys.exit('the fixture read answered nothing')
print(json.loads(raw).get('units', 'missing'))
"
}
