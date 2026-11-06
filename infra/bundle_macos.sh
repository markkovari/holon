#!/usr/bin/env bash
# Build and package Holon native macOS application bundles:
#   - Holon.app (GPUI visual board with autonomous agent explorer, sessions, & rollback)
#   - Holon Voice.app (Conversational companion menu bar app)
#
# Usage:
#   ./infra/bundle_macos.sh            Build both apps into release bundles
#   ./infra/bundle_macos.sh --install  Build and install both into /Applications (or ~/Applications)
#   ./infra/bundle_macos.sh --board    Build Holon.app only
#   ./infra/bundle_macos.sh --voice    Build Holon Voice.app only
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"

INSTALL_FLAG=""
BUILD_BOARD=1
BUILD_VOICE=1

for arg in "$@"; do
  case "$arg" in
    --install)
      INSTALL_FLAG="--install"
      ;;
    --board)
      BUILD_BOARD=1
      BUILD_VOICE=0
      ;;
    --voice)
      BUILD_BOARD=0
      BUILD_VOICE=1
      ;;
    --help|-h)
      echo "Usage: $0 [--board] [--voice] [--install]"
      exit 0
      ;;
  esac
done

if [[ "$BUILD_BOARD" -eq 1 ]]; then
  echo "=================================================="
  echo "==> Building Holon.app (GPUI Board UI)..."
  echo "=================================================="
  "$ROOT/gpui-console/build_app.sh" $INSTALL_FLAG
fi

if [[ "$BUILD_VOICE" -eq 1 ]]; then
  echo "=================================================="
  echo "==> Building Holon Voice.app (Voice Companion)..."
  echo "=================================================="
  "$ROOT/voice/build_app.sh" $INSTALL_FLAG
fi

echo ""
echo "=================================================="
echo " macOS Application Bundles Ready!"
echo "=================================================="
if [[ -n "$INSTALL_FLAG" ]]; then
  echo "Applications installed. Launch them from Spotlight or Finder:"
  echo "  - Holon: 'open -a Holon' or Spotlight 'Holon'"
  if [[ "$BUILD_VOICE" -eq 1 ]]; then
    echo "  - Holon Voice: 'open -a \"Holon Voice\"' or Spotlight 'Holon Voice'"
  fi
else
  echo "Bundles built in:"
  if [[ "$BUILD_BOARD" -eq 1 ]]; then
    echo "  - Holon.app: $ROOT/gpui-console/target/release/Holon.app"
  fi
  if [[ "$BUILD_VOICE" -eq 1 ]]; then
    echo "  - Holon Voice.app: $ROOT/target/release/Holon Voice.app"
  fi
  echo ""
  echo "To install both directly to /Applications (or ~/Applications):"
  echo "  $0 --install"
fi
