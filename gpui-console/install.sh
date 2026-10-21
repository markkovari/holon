#!/usr/bin/env sh
# Build the console and everything it starts, then put it on PATH as `holon-console`
# and in ~/Applications as "Holon Console". Both run this checkout's build, so a later
# `cargo build --release` here is what they open next.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

command -v nats-server >/dev/null || { echo "install.sh: needs nats-server (brew install nats-server)" >&2; exit 1; }

(cd "$ROOT/host" && cargo build --release --bin comp-host)
(cd "$ROOT/reconciler" && cargo build --release --bin comp-reconciler --bin comp-ingress --bin comp-stub --bin comp-agentd)
(cd "$ROOT/gpui-console" && cargo build --release --bin gpui-console)

BIN="${HOLON_BIN_DIR:-$HOME/.local/bin}"
mkdir -p "$BIN"
# Finder starts an app with a bare PATH, and the console spawns nats-server and comp-* by name.
cat > "$BIN/holon-console" <<EOF
#!/bin/sh
export PATH="\$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:\$PATH"
# Opens on the agent board (Sessions, Agents, Explore, Map, Keys), not the older console view.
export HOLON_CONSOLE_BOARD="\${HOLON_CONSOLE_BOARD:-1}"
exec "$ROOT/gpui-console/target/release/gpui-console" "\$@"
EOF
chmod +x "$BIN/holon-console"

if [ "$(uname -s)" = Darwin ]; then
  APP="$HOME/Applications/Holon Console.app/Contents"
  mkdir -p "$APP/MacOS"
  cat > "$APP/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>holon-console</string>
  <key>CFBundleIdentifier</key><string>io.holon.console</string>
  <key>CFBundleName</key><string>Holon Console</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
  cat > "$APP/MacOS/holon-console" <<EOF
#!/bin/sh
mkdir -p "\$HOME/.holon-console-state"
exec "$BIN/holon-console" "\$@" >>"\$HOME/.holon-console-state/console.log" 2>&1
EOF
  chmod +x "$APP/MacOS/holon-console"
  echo "installed: $BIN/holon-console and ~/Applications/Holon Console.app"
else
  echo "installed: $BIN/holon-console"
fi
case ":$PATH:" in *":$BIN:"*) ;; *) echo "note: $BIN is not on your PATH" ;; esac
