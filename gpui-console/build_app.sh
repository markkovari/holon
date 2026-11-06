#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"

export PATH="$HOME/.cargo/bin:$PATH"

INSTALL=0
if [[ "${1:-}" == "--install" ]]; then
  INSTALL=1
fi

echo "==> Building agent-board binary (release)..."
cargo build --release --manifest-path "$DIR/Cargo.toml" -p agent-board

TARGET_BIN="$DIR/target/release/agents-board"
APP_DIR="$DIR/target/release/Holon.app"

echo "==> Creating macOS application bundle: $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

# 1. Copy binary
cp "$TARGET_BIN" "$APP_DIR/Contents/MacOS/Holon"
chmod +x "$APP_DIR/Contents/MacOS/Holon"

# 2. Generate multi-resolution AppIcon.icns from mascot PNG if available
MASCOT_PNG="$ROOT/docs/media/holon-mascot.png"
if [[ -f "$MASCOT_PNG" ]] && command -v sips >/dev/null && command -v iconutil >/dev/null; then
  echo "==> Generating AppIcon.icns from $MASCOT_PNG..."
  ICONSET_DIR="$(mktemp -d)/Holon.iconset"
  mkdir -p "$ICONSET_DIR"
  sips -z 16 16     "$MASCOT_PNG" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null 2>&1
  sips -z 32 32     "$MASCOT_PNG" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null 2>&1
  sips -z 32 32     "$MASCOT_PNG" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null 2>&1
  sips -z 64 64     "$MASCOT_PNG" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null 2>&1
  sips -z 128 128   "$MASCOT_PNG" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null 2>&1
  sips -z 256 256   "$MASCOT_PNG" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null 2>&1
  sips -z 256 256   "$MASCOT_PNG" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null 2>&1
  sips -z 512 512   "$MASCOT_PNG" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null 2>&1
  sips -z 512 512   "$MASCOT_PNG" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null 2>&1
  sips -z 1024 1024 "$MASCOT_PNG" --out "$ICONSET_DIR/icon_512x512@2x.png" >/dev/null 2>&1
  iconutil -c icns "$ICONSET_DIR" -o "$APP_DIR/Contents/Resources/AppIcon.icns"
  rm -rf "$(dirname "$ICONSET_DIR")"
fi

# 3. Write Info.plist
cat > "$APP_DIR/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>io.holon.board</string>
    <key>CFBundleName</key>
    <string>Holon</string>
    <key>CFBundleDisplayName</key>
    <string>Holon</string>
    <key>CFBundleExecutable</key>
    <string>Holon</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

# 4. Sign the bundle with ad-hoc signature
if command -v codesign >/dev/null; then
  echo "==> Signing application bundle..."
  codesign --force --deep --sign - --identifier io.holon.board "$APP_DIR"
fi

echo "==> Built $APP_DIR successfully."

# 5. Optionally install to /Applications or ~/Applications
if [[ "$INSTALL" -eq 1 ]]; then
  DEST_DIR="/Applications"
  if [[ ! -w "$DEST_DIR" ]]; then
    DEST_DIR="$HOME/Applications"
    mkdir -p "$DEST_DIR"
  fi
  echo "==> Installing to $DEST_DIR/Holon.app..."
  rm -rf "$DEST_DIR/Holon.app"
  cp -R "$APP_DIR" "$DEST_DIR/Holon.app"
  echo "==> Installed Holon.app to $DEST_DIR"
  echo "    You can launch it via Spotlight (Cmd + Space -> Holon) or with:"
  echo "    open \"$DEST_DIR/Holon.app\""
else
  echo ""
  echo "To launch right now:"
  echo "  open \"$APP_DIR\""
  echo ""
  echo "To install to your Applications folder:"
  echo "  $0 --install"
fi
