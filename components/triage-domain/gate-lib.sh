# The triage API's gates, named. Everything they do lives one directory up, shared
# with every other app's gates; this file is only which crate is under test.
#
# It stays here so the four `e2e-*.sh` scripts keep sourcing the path they always
# did — moving the library was not meant to be a change to any gate.
GATE_CRATE=triage-domain
GATE_APP=triage
GATE_PKGS="-p triage-domain -p record-store -p id-generate -p pii-redact -p fsm-workflow -p csv"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh
