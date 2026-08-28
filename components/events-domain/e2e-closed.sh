#!/usr/bin/env bash
# The fixture is off unless somebody turns it on.
#
# Every other gate in this app sets `allow-test-routes=1` and then uses `/test/seed`
# to get bearers. This one is the negative control for that switch, and it exists
# because the switch did not: the fixture was compiled into the artifact that was
# deployed to a real box, the SPA called it on load, and the app had no login screen
# because it did not need one. Neither did anyone else who could reach the URL.
#
# So this gate runs the SAME artifact with the flag ABSENT and requires that the
# route is gone — and that the app still works, through the front door.
set -uo pipefail
# shellcheck source=components/events-domain/gate-lib.sh
. components/events-domain/gate-lib.sh

# Undo what gate-lib.sh sets: this gate is the one that must NOT have the fixture.
# It does name a bootstrap organizer, which is the other half of the same question —
# with no fixture, who opens the first event?
BOSS_EMAIL="boss$$@example.test"
GATE_CONFIG="--config organizer-emails=$BOSS_EMAIL"

gate_require_tools
gate_build
gate_compose

trap gate_cleanup EXIT
gate_serve

# --- the fixture is not reachable ---------------------------------------------
expect_post 404 /test/seed '{}' "the fixture must be 404 when allow-test-routes is not set"
expect_get 404 "/test/events/anything" "every /test route goes with it"

# --- and the front door works --------------------------------------------------
EMAIL="ada$$@example.test"
REG=$(post /api/register "{\"email\":\"$EMAIL\",\"password\":\"correct-horse\"}")
TOKEN=$(printf '%s' "$REG" | field token)
[ -n "$TOKEN" ] || fail "registering did not return a token: $REG"

expect_post 409 /api/register "{\"email\":\"$EMAIL\",\"password\":\"correct-horse\"}" \
  "the same email twice is a 409"
expect_post 400 /api/register '{"email":"nope","password":"correct-horse"}' \
  "an address with no @ is a 400"
expect_post 400 /api/register "{\"email\":\"x$$@example.test\",\"password\":\"short\"}" \
  "a password under 8 characters is a 400"

LOGIN=$(post /api/login "{\"email\":\"$EMAIL\",\"password\":\"correct-horse\"}")
case "$LOGIN" in *'"attendee"'*) ;; *) fail "login must report the caller's roles so the SPA knows which screen to draw: $LOGIN" ;; esac
expect_post 401 /api/login "{\"email\":\"$EMAIL\",\"password\":\"wrong-password\"}" \
  "a bad password is 401"

# --- a registered attendee is an ATTENDEE, not an organizer ---------------------
aexpect_post "$TOKEN" 403 /api/events '{"title":"self-promoted","starts_at":"2026-10-01T18:00:00Z","capacity":5}' \
  "signing up must not grant event:write — a person cannot claim a role by asking for one"
aexpect_get "$TOKEN" 200 /api/tickets "but they can see their own tickets"

# --- the deployment can name its first organizer ---------------------------------
#
# Without this a fresh box has nobody who may open an event and no organizer to
# grant the role — a deadlock, not a security property. The list is CONFIG, so a
# person asking for a role still cannot put themselves on it.
BOSS=$(post /api/register "{\"email\":\"$BOSS_EMAIL\",\"password\":\"correct-horse\"}" | field token)
[ -n "$BOSS" ] || fail "the named organizer could not register"
aexpect_post "$BOSS" 201 /api/events '{"title":"opened by the named organizer","starts_at":"2026-10-01T18:00:00Z","capacity":5}' \
  "an email in organizer-emails must get event:write on registration, or a fresh box has nobody who can open anything"

echo "PASSED: the fixture is closed, the front door is open, and signing up does not make you an organizer"
