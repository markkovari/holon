# The ticketing gates, named — plus the one thing this app needs that no gate before
# it did: every route is behind a bearer.
#
# `components/gate-lib.sh` has `post`/`get`/`expect_post`, and none of them can send
# a header. Rather than each of the five gates growing its own `curl -H`, the auth
# variants live here once. They are named `apost`/`aget`/`aexpect_*` so a gate that
# forgets the token gets a 401 from the plain helper rather than silently passing
# something unauthenticated.
GATE_CRATE=events-domain
GATE_APP=events
GATE_PKGS="-p events-domain -p record-store -p id-generate -p quota -p qr -p fsm-workflow -p auth-guard -p rate-limiter -p audit-log"

# The fixture is a gate's tool and is OFF unless this says otherwise — see
# `test_routes_allowed` in src/lib.rs. It was compiled into the artifact that got
# deployed, where the SPA called it on load and the app therefore had no login
# screen; so did anybody else who could reach the URL.
GATE_CONFIG="${GATE_CONFIG:-} --config allow-test-routes=1"
# `upload-policy` reads these and answers `check`. Without them every poster is
# refused, and the refusal is correct — an empty allowlist allows nothing.
GATE_CONFIG="$GATE_CONFIG --config allowed-types=image/png,image/jpeg,image/webp --config max-size=2097152"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

# --- the same helpers, with a bearer -----------------------------------------
apost() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$3" "$B$2"; }
apcode() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$3" "$B$2"; }
aget() { curl -s -H "authorization: Bearer $1" "$B$2"; }
agcode() { curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $1" "$B$2"; }
adelete() { curl -s -o /dev/null -w '%{http_code}' -X DELETE -H "authorization: Bearer $1" "$B$2"; }
apatch() { curl -s -o /dev/null -w '%{http_code}' -X PATCH -H 'content-type: application/json' -H "authorization: Bearer $1" -d "$3" "$B$2"; }

aexpect_post() { # aexpect_post <token> <code> <path> <body> <message>
  local tok="$1" want="$2" path="$3" body="$4" msg="$5" got
  got=$(apcode "$tok" "$path" "$body")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}

aexpect_get() { # aexpect_get <token> <code> <path> <message>
  local tok="$1" want="$2" path="$3" msg="$4" got
  got=$(agcode "$tok" "$path")
  [ "$got" = "$want" ] || fail "$msg (got $got, wanted $want)"
}

# Seed, and export what every gate needs: three bearers and one event.
#
# The fixture registers the people and hands their tokens back because a gate cannot
# mint one — `auth-guard` signs them and the secret is inside the composition. This
# is also why the fixture is idempotent: five gates call it.
events_seed() {
  local raw
  raw=$(post /test/seed '{}')
  EVENT_ID=$(printf '%s' "$raw" | python3 -c "import sys,json;print(json.load(sys.stdin).get('event_id',''))" 2>/dev/null)
  CONTESTED_ID=$(printf '%s' "$raw" | python3 -c "import sys,json;print(json.load(sys.stdin).get('contested_event_id',''))" 2>/dev/null)
  ORGANIZER=$(printf '%s' "$raw" | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['organizer']['token'])" 2>/dev/null)
  ATTENDEE=$(printf '%s' "$raw" | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['attendee']['token'])" 2>/dev/null)
  OTHER=$(printf '%s' "$raw" | python3 -c "import sys,json;print(json.load(sys.stdin)['tokens']['other']['token'])" 2>/dev/null)
  [ -n "$EVENT_ID" ] && [ -n "$ORGANIZER" ] && [ -n "$ATTENDEE" ] && [ -n "$OTHER" ] || {
    fail "the fixture did not come back with an event and three tokens: $raw"
  }
  export EVENT_ID CONTESTED_ID ORGANIZER ATTENDEE OTHER
}

# Read a stored document through the router's scaffold route, so a gate can see what
# a part WROTE without going through the part that owns the read route — which is a
# stub while this part is judged alone.
stored() { get "/test/$1/$2"; }

# Every gate asserts this: a route behind auth must refuse an anonymous caller. It
# is one line per gate and it catches the part that read the contract's routes and
# skipped its authorisation table.
expect_unauthenticated() { # expect_unauthenticated <method> <path> [body]
  local got
  case "$1" in
    GET) got=$(curl -s -o /dev/null -w '%{http_code}' "$B$2") ;;
    *) got=$(curl -s -o /dev/null -w '%{http_code}' -X "$1" -H 'content-type: application/json' -d "${3:-\{\}}" "$B$2") ;;
  esac
  [ "$got" = "401" ] || fail "$1 $2 answered $got to a request with no bearer — the contract says 401"
}
