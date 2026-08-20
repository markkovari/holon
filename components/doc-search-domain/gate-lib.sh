# docsearch:agent's gates, named. Everything they do lives in `components/gate-lib.sh`.
GATE_CRATE=doc-search-domain
GATE_APP=docsearch
# auth-guard's own world imports ratelimit:guard and audit:log, so the composition needs
# them built even though nothing here calls either. Omit them and the artifact composes
# with an unimplemented import and traps at the first request.
GATE_PKGS="-p doc-search-domain -p auth-guard -p rate-limiter -p audit-log -p otp \
-p quota -p search-index -p cache -p ai-inference -p anthropic-provider -p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

# Every part authorizes, so every gate asserts it.
docs_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}

# A TOTP code for a secret, from python's standard library alone. The gate has to be
# able to present a CORRECT code: a step-up that is only ever tested with a wrong one is
# a step-up nobody has checked.
totp_now() { # totp_now <base32-secret>
  python3 - "$1" <<'PY'
import base64, hmac, hashlib, struct, sys, time
secret = sys.argv[1].strip().upper()
secret += "=" * (-len(secret) % 8)
key = base64.b32decode(secret, casefold=True)
counter = int(time.time()) // 30
digest = hmac.new(key, struct.pack(">Q", counter), hashlib.sha1).digest()
offset = digest[-1] & 0x0F
code = (struct.unpack(">I", digest[offset:offset + 4])[0] & 0x7FFFFFFF) % 10**6
print(f"{code:06d}")
PY
}
