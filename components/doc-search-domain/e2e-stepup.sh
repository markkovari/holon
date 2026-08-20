#!/usr/bin/env bash
# docsearch:agent — the step-up part
#
# Tested with a CORRECT code, which is the point. A step-up only ever tried with a wrong
# code is a step-up nobody has checked: the branch that always answers `bad_code` passes
# every negative test there is. `gate-lib.sh` computes a real TOTP from the provisioned
# secret out of python's standard library, so the happy path is a real second factor.
#
# The other half is what a WRONG code must not do: not verify, and not un-verify. A part
# that clears the mark on a failed attempt hands anyone a logout for someone else.
set -uo pipefail
# shellcheck source=components/doc-search-domain/gate-lib.sh
. components/doc-search-domain/gate-lib.sh

gate_require_tools
gate_build
gate_compose

docs_requires_auth
gate_requires_capability "otp:totp/authenticator" \
  "TOTP is a solved problem in this repository — \`provision\` and \`verify\` with a skew window, not an HMAC this part writes"

trap gate_cleanup EXIT
gate_serve

token() { post /test/token "{\"subject\":\"$1\"${2:+,\"scopes\":$2}}" | field token; }
T=$(token ada)
[ -n "$T" ] || fail "POST /test/token returned no token — the scaffold is broken, not the part"
auth() { curl -s -H "authorization: Bearer $T" "$@"; }
mfa() { auth "$B/api/mfa"; }
verify() { curl -s -X POST -H 'content-type: application/json' -H "authorization: Bearer $T" \
  -d "{\"code\":\"$1\"}" "$B/api/mfa/verify"; }
verify_code() { curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
  -H "authorization: Bearer $T" -d "{\"code\":\"$1\"}" "$B/api/mfa/verify"; }

# --- the refusals ---------------------------------------------------------------
GOT=$(curl -s -o /dev/null -w '%{http_code}' -X POST -d '{"code":"000000"}' "$B/api/mfa/verify")
[ "$GOT" = 401 ] || fail "verifying with no bearer must be 401, got $GOT"
GOT=$(verify_code 000000)
[ "$GOT" = 409 ] || fail "verifying before enrolling must be 409 not_enrolled, got $GOT (a 401 here says the code was wrong, which is not what happened)"

# --- enrol ----------------------------------------------------------------------
ENROL=$(curl -s -X POST -H "authorization: Bearer $T" "$B/api/mfa/enroll")
SECRET=$(printf '%s' "$ENROL" | field secret)
python3 - "$ENROL" <<'PY' || fail "enrolling did not return a usable secret and uri"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
s = d.get("secret") or ""
assert len(s) >= 16, f"a TOTP secret is base32 and not short: {d}"
uri = d.get("uri") or ""
assert uri.startswith("otpauth://"), f"the uri must be the otpauth:// one an authenticator app can read: {uri!r}"
assert "docsearch" in uri, f"the issuer belongs in the uri: {uri!r}"
PY
python3 - "$(mfa)" <<'PY' || fail "after enrolling, the part must report enrolled and NOT verified"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("enrolled") is True, f"enrolled must be true after enrolling: {d}"
assert d.get("verified") is False, f"enrolling is not verifying: {d}"
PY

# --- a wrong code: refused, and it changes nothing ------------------------------
GOT=$(verify_code 000000)
[ "$GOT" = 401 ] || fail "a wrong code must be 401 bad_code, got $GOT"
python3 - "$(mfa)" <<'PY' || fail "a wrong code must not verify the session"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("verified") is False, "a wrong code verified the session"
PY

# --- the real thing -------------------------------------------------------------
CODE=$(totp_now "$SECRET")
[ -n "$CODE" ] || fail "the gate could not compute a TOTP code — that is the gate, not the part"
OK=$(verify "$CODE")
python3 - "$OK" <<'PY' || fail "a correct TOTP code was refused — the code was computed from the secret this part provisioned"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("verified") is True, f"a correct code must verify: {d}"
PY
python3 - "$(mfa)" <<'PY' || fail "after a correct code the part must report the session verified"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("verified") is True, "a verified session reads as unverified"
PY

# A wrong code AFTER verifying must not undo it: that would be a logout anyone can cause.
verify_code 000000 >/dev/null
python3 - "$(mfa)" <<'PY' || fail "a wrong code un-verified an already verified session"
import json, sys
assert json.loads(sys.argv[1] or "{}").get("verified") is True, \
    "a failed attempt cleared a verified step-up — anyone who knows a subject can log them out"
PY

# --- re-enrolling is a new authenticator, and it has not been used --------------
curl -s -o /dev/null -X POST -H "authorization: Bearer $T" "$B/api/mfa/enroll"
python3 - "$(mfa)" <<'PY' || fail "re-enrolling left the old verification standing"
import json, sys
raw = (sys.argv[1] or "").strip()
assert raw, "the route answered an empty body — it is not implemented, or it trapped"
d = json.loads(raw)
assert d.get("enrolled") is True and d.get("verified") is False, \
    f"a new secret has not been used yet, so verified must go back to false: {d}"
PY

echo "docsearch:agent — the step-up part: passed"
