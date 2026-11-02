#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"

export PATH="$HOME/.cargo/bin:$PATH"

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
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$DIR/tray/HolonVoiceTray" "$APP_DIR/Contents/MacOS/HolonVoiceTray"

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
