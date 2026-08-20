# invoice:copilot's gates, named. Everything they do lives in `components/gate-lib.sh`.
GATE_CRATE=invoice-copilot-domain
GATE_APP=invoice
# audit:log is auth-guard's import, not this app's, and the composition still needs it.
GATE_PKGS="-p invoice-copilot-domain -p auth-guard -p rate-limiter -p audit-log \
-p money -p ledger -p idempotency-guard -p ai-inference -p anthropic-provider \
-p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

inv_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}
