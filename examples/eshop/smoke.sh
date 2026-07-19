#!/usr/bin/env bash
# eshop smoke — the full eShopOnDapr checkout choreography, asserted step by step:
#   register/login -> browse catalog -> fill basket -> checkout(202)
#   -> UserCheckoutAccepted -> order submitted -> grace expires
#   -> stock validated -> payment simulated -> order PAID
#   -> stock decremented -> basket cleared
#
# Direct mode (native hosts):   ./smoke.sh
# Gateway mode (SPA/k8s edge):  GATEWAY=http://127.0.0.1:8080 ./smoke.sh
# Knobs: expects ordering to run with CFG_GRACE_PERIOD_SECS small (e.g. 3).
set -euo pipefail

IDENTITY=${IDENTITY:-http://127.0.0.1:3105}
CATALOG=${CATALOG:-http://127.0.0.1:3101}
BASKET=${BASKET:-http://127.0.0.1:3102}
ORDERING=${ORDERING:-http://127.0.0.1:3103}
PAYMENT=${PAYMENT:-http://127.0.0.1:3104}
if [ -n "${GATEWAY:-}" ]; then
  IDENTITY="$GATEWAY/api/identity"; CATALOG="$GATEWAY"; BASKET="$GATEWAY"
  ORDERING="$GATEWAY"; PAYMENT="$GATEWAY"
fi

say()  { printf '\033[1;34m== %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mFAIL: %s\033[0m\n' "$*"; exit 1; }
jget() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

EMAIL="shopper-$RANDOM@eshop.test"

say "register + login ($EMAIL)"
curl -sf -X POST "$IDENTITY/register" -d "{\"email\":\"$EMAIL\",\"password\":\"P@ssword1\"}" >/dev/null
TOKEN=$(curl -sf -X POST "$IDENTITY/login" -d "{\"email\":\"$EMAIL\",\"password\":\"P@ssword1\"}" | jget "d['access_token']")
[ -n "$TOKEN" ] || fail "no token"
AUTH="Authorization: Bearer $TOKEN"

say "browse catalog"
ITEM=$(curl -sf "$CATALOG/api/catalog/items?pageSize=1")
PRODUCT=$(echo "$ITEM" | jget "d['data'][0]['id']")
NAME=$(echo "$ITEM" | jget "d['data'][0]['name']")
PRICE=$(echo "$ITEM" | jget "d['data'][0]['price']")
STOCK0=$(curl -sf "$CATALOG/api/catalog/items/$PRODUCT" | jget "d['availableStock']")
echo "   product: $NAME ($PRODUCT) price=$PRICE stock=$STOCK0"

say "fill basket (qty 2)"
curl -sf -X POST "$BASKET/api/basket" -H "$AUTH" -d "{\"items\":[{\"productId\":\"$PRODUCT\",\"productName\":\"$NAME\",\"unitPrice\":$PRICE,\"quantity\":2}]}" >/dev/null
N=$(curl -sf "$BASKET/api/basket" -H "$AUTH" | jget "len(d['items'])")
[ "$N" = "1" ] || fail "basket not stored"

say "checkout (expect 202)"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASKET/api/basket/checkout" -H "$AUTH" \
  -d '{"city":"Redmond","street":"1 Wasm Way","state":"WA","country":"USA","zipCode":"98052","cardNumber":"4012888888881881","cardHolderName":"Shopper","cardExpiration":"12/28","cardSecurityNumber":"123","cardTypeId":1}')
[ "$CODE" = "202" ] || fail "checkout returned $CODE"

say "pump the choreography until the order is paid"
STATUS=""
for i in $(seq 1 30); do
  curl -sf -X POST "$ORDERING/internal/pump" >/dev/null
  curl -sf -X POST "$CATALOG/internal/pump"  >/dev/null
  curl -sf -X POST "$PAYMENT/internal/pump"  >/dev/null
  curl -sf -X POST "$BASKET/internal/pump"   >/dev/null
  STATUS=$(curl -sf "$ORDERING/api/orders" -H "$AUTH" | jget "d['orders'][0]['status'] if d['orders'] else ''")
  echo "   tick $i: order status = ${STATUS:-<none>}"
  [ "$STATUS" = "paid" ] && break
  [ "$STATUS" = "cancelled" ] && fail "order was cancelled"
  sleep 1
done
[ "$STATUS" = "paid" ] || fail "order never reached paid (last: $STATUS)"

say "assert stock decremented + basket cleared + history"
ORDER_ID=$(curl -sf "$ORDERING/api/orders" -H "$AUTH" | jget "d['orders'][0]['id']")
STOCK1=$(curl -sf "$CATALOG/api/catalog/items/$PRODUCT" | jget "d['availableStock']")
[ "$STOCK1" = "$((STOCK0 - 2))" ] || fail "stock $STOCK0 -> $STOCK1, expected $((STOCK0 - 2))"
N=$(curl -sf "$BASKET/api/basket" -H "$AUTH" | jget "len(d['items'])")
[ "$N" = "0" ] || fail "basket not cleared"
HOPS=$(curl -sf "$ORDERING/api/orders/$ORDER_ID" -H "$AUTH" | jget "'>'.join(h['to'] for h in d['history'])")
echo "   lifecycle: submitted>$HOPS"
TOTAL=$(curl -sf "$ORDERING/api/orders/$ORDER_ID" -H "$AUTH" | jget "d['total']")
[ "$TOTAL" = "$((PRICE * 2))" ] || fail "total $TOTAL != $((PRICE * 2))"

say "grace-period cancel on a second order"
curl -sf -X POST "$BASKET/api/basket" -H "$AUTH" -d "{\"items\":[{\"productId\":\"$PRODUCT\",\"productName\":\"$NAME\",\"unitPrice\":$PRICE,\"quantity\":1}]}" >/dev/null
curl -sf -X POST "$BASKET/api/basket/checkout" -H "$AUTH" \
  -d '{"city":"Redmond","street":"1 Wasm Way","state":"WA","country":"USA","zipCode":"98052","cardNumber":"4012888888881881","cardHolderName":"Shopper","cardExpiration":"12/28","cardSecurityNumber":"123","cardTypeId":1}' >/dev/null
curl -sf -X POST "$ORDERING/internal/pump" >/dev/null
ORDER2=$(curl -sf "$ORDERING/api/orders" -H "$AUTH" | jget "[o['id'] for o in d['orders'] if o['status']=='submitted'][0]")
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$ORDERING/api/orders/$ORDER2/cancel" -H "$AUTH")
[ "$CODE" = "200" ] || fail "cancel inside grace window returned $CODE"
ST=$(curl -sf "$ORDERING/api/orders/$ORDER2" -H "$AUTH" | jget "d['status']")
[ "$ST" = "cancelled" ] || fail "order2 status $ST != cancelled"

printf '\033[1;32mSMOKE PASSED\033[0m — checkout choreography green end to end\n'
