#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"

export PATH="$HOME/.cargo/bin:$PATH"

INSTALL=0
if [[ "${1:-}" == "--install" ]]; then
  INSTALL=1
fi

# 1. Build Rust holon-voice binary
cargo build --release -p holon-voice
codesign --force --sign - \
  --entitlements "$DIR/entitlements.plist" \
  --requirements '=designated => identifier "io.holon.voice"' \
  "$ROOT/target/release/holon-voice"

# 2. Compile Swift HolonVoiceTray binary
mkdir -p /tmp/clang-cache
swiftc -module-cache-path /tmp/clang-cache -O "$DIR/tray/HolonVoiceTray.swift" -o "$DIR/tray/HolonVoiceTray"

# 3. Create Holon Voice.app bundle
APP_DIR="$ROOT/target/release/Holon Voice.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$DIR/tray/HolonVoiceTray" "$APP_DIR/Contents/MacOS/HolonVoiceTray"

# Generate multi-resolution AppIcon.icns from mascot PNG if available
MASCOT_PNG="$ROOT/docs/media/holon-mascot.png"
if [[ -f "$MASCOT_PNG" ]] && command -v sips >/dev/null && command -v iconutil >/dev/null; then
  ICONSET_DIR="$(mktemp -d)/HolonVoice.iconset"
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

cat > "$APP_DIR/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>io.holon.voice.tray</string>
    <key>CFBundleName</key>
    <string>Holon Voice</string>
    <key>CFBundleDisplayName</key>
    <string>Holon Voice</string>
    <key>CFBundleExecutable</key>
    <string>HolonVoiceTray</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
EOF

# 4. Sign the tray app bundle
codesign --force --deep --sign - --identifier io.holon.voice.tray "$APP_DIR"
echo "Built and signed $APP_DIR successfully."

# 5. Optionally install to /Applications or ~/Applications
if [[ "$INSTALL" -eq 1 ]]; then
  DEST_DIR="/Applications"
  if [[ ! -w "$DEST_DIR" ]]; then
    DEST_DIR="$HOME/Applications"
    mkdir -p "$DEST_DIR"
  fi
  echo "==> Installing to $DEST_DIR/Holon Voice.app..."
  rm -rf "$DEST_DIR/Holon Voice.app"
  cp -R "$APP_DIR" "$DEST_DIR/Holon Voice.app"
  echo "==> Installed Holon Voice.app to $DEST_DIR"
fi
