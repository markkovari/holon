# triage:assist's gates, named. Everything they do lives in `components/gate-lib.sh`,
# shared with every other app's gates; this file is only which crate is under test.
#
# The build list is every crate the composition needs, and `anthropic-provider` is on
# it deliberately: `ai-inference` is orchestration over `llm:inference`, and the
# provider is what makes the call real. `mock-provider` exports the same interface —
# leaving both built and letting the catalogue pick would decide the most important
# thing about this gate by alphabetical order.
GATE_CRATE=triage-assist-domain
GATE_APP=triage-assist
GATE_PKGS="-p triage-assist-domain -p auth-guard -p rate-limiter -p pii-redact \
-p ai-inference -p anthropic-provider -p audit-log -p record-store -p id-generate"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

# Every part authorizes, so every gate asserts it. Named once here rather than
# copied into three scripts.
assist_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}
