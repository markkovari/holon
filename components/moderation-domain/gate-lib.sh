# moderation:queue's gates, named. Everything they do lives in `components/gate-lib.sh`.
GATE_CRATE=moderation-domain
GATE_APP=moderation
GATE_PKGS="-p moderation-domain -p auth-guard -p rate-limiter -p policy-guard \
-p event-bus -p ai-inference -p anthropic-provider -p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

mod_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}
