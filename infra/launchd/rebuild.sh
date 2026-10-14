#!/usr/bin/env bash
# Rebuild the agent runtime and the Matrix bridge, sign what macOS grants permissions to, and
# restart them under launchd.
#
#   ./rebuild.sh               build, sign, restart both
#   ./rebuild.sh --no-restart  build and sign only
#
# Why it signs: macOS remembers a privacy grant (Calendar, ...) by the program's code identity.
# A local build is signed "ad hoc", whose identity is a hash of the binary itself, so every
# rebuild is a new app and macOS asks again. Signed with a certificate and a fixed identifier the
# identity is "this identifier, signed by this certificate" and survives rebuilds: allow it once.
#
# The identity is the first "Apple Development" certificate in the keychain; set
# HOLON_SIGN_IDENTITY to use another (`security find-identity -v -p codesigning` lists them).
# Without one it still builds and restarts, unsigned, and says that macOS will ask again.
# codesign may show a keychain prompt the first time: choose "Always Allow".
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
UID_="$(id -u)"
RESTART=1
[ "${1:-}" = "--no-restart" ] && RESTART=0

IDENTITY="${HOLON_SIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi

echo "== build"
(cd "$REPO/agent-runtime" && cargo build --release)
(cd "$REPO/agent-matrix" && cargo build --release)

# Sign a COPY and rename it over the original: the rename is atomic and leaves a running process
# on its old file, where signing in place would invalidate the pages it has mapped.
sign() {
  local bin="$1" identifier="$2" tmp
  [ -f "$bin" ] || return 0
  tmp="$(mktemp "$bin.signing.XXXXXX")"
  cp -p "$bin" "$tmp"
  codesign --force --sign "$IDENTITY" --identifier "$identifier" "$tmp"
  mv "$tmp" "$bin"
  echo "signed $bin as $identifier"
}

if [ -n "$IDENTITY" ]; then
  echo "== sign with: $IDENTITY"
  # the runtime is what launchd runs, so it is what Calendar access is granted to; the helper is
  # the program it starts for that
  sign "$REPO/agent-runtime/target/release/agent-runtime" io.holon.agent-runtime
  sign "$HOME/.holon-agents/holon-calendar" io.holon.calendar
else
  echo "== NOT signed: no code-signing identity found (set HOLON_SIGN_IDENTITY)."
  echo "   macOS will ask for Calendar access again after this rebuild."
fi

if [ "$RESTART" = 1 ]; then
  echo "== restart"
  launchctl kickstart -k "gui/$UID_/io.holon.runtime"
  sleep 3
  launchctl kickstart -k "gui/$UID_/io.holon.bridge"
fi

echo "== identity now"
codesign -dvv "$REPO/agent-runtime/target/release/agent-runtime" 2>&1 | grep -E "^(Identifier|Authority|Signature|TeamIdentifier)" | head -4
codesign -d -r- "$REPO/agent-runtime/target/release/agent-runtime" 2>&1 | grep designated | cut -c1-160
