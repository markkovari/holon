#!/usr/bin/env sh
# Put the agent board on PATH as `holon-console` and in ~/Applications as "Holon Console": the tabs
# (Sessions, Agents, Explore, Map, Keys) on the one agent runtime, the one Element talks to
# (infra/launchd, 127.0.0.1:18017). Both run this checkout's release build.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

(cd "$ROOT/gpui-console" && cargo build --release -p agent-board --bin agents-board)

BIN="${HOLON_BIN_DIR:-$HOME/.local/bin}"
mkdir -p "$BIN"
cat > "$BIN/holon-console" <<EOS
#!/bin/sh
exec "$ROOT/gpui-console/target/release/agents-board" --runtime "\${HOLON_RUNTIME_URL:-http://127.0.0.1:18017}" "\$@"
EOS
chmod +x "$BIN/holon-console"

if [ "$(uname -s)" = Darwin ]; then
  APP="$HOME/Applications/Holon Console.app/Contents"
  mkdir -p "$APP/MacOS"
  cat > "$APP/Info.plist" <<EOS
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>holon-console</string>
  <key>CFBundleIdentifier</key><string>io.holon.console</string>
  <key>CFBundleName</key><string>Holon Console</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOS
  cat > "$APP/MacOS/holon-console" <<EOS
#!/bin/sh
mkdir -p "\$HOME/Library/Logs/holon"
exec "$BIN/holon-console" "\$@" >>"\$HOME/Library/Logs/holon/console.log" 2>&1
EOS
  chmod +x "$APP/MacOS/holon-console"
  echo "installed: $BIN/holon-console and ~/Applications/Holon Console.app"
else
  echo "installed: $BIN/holon-console"
fi
