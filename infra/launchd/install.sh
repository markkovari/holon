#!/usr/bin/env bash
# Keep the agent stack running across logins and crashes, as launchd user agents.
#
#   ./install.sh install    write the plists and (re)start every service
#   ./install.sh uninstall  stop and remove them
#   ./install.sh status     what is running
#
# Services (label io.holon.<name>): fm (Apple model), qwen (4B model), qwen-large (27B, slower, ~15 GB), embed (EmbeddingGemma 2),
# runtime (the agents), bridge (Matrix). Logs: ~/Library/Logs/holon/<name>.log. Nothing secret
# goes in a plist: tokens stay in ~/.holon-agents and ~/.holon-matrix, where the programs read them.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
AGENTS="$HOME/Library/LaunchAgents"
LOGS="$HOME/Library/Logs/holon"
UID_="$(id -u)"
SERVICES=(fm qwen qwen-large embed runtime bridge)
# Big local models: installed but not started. The runtime starts one when a model chain needs it
# (DeepSeek unavailable or not good enough; or, for embed, a recall or routing call) and stops it
# after 10 idle minutes.
LAZY=" qwen qwen-large embed "
NO_THINKING='{"enable_thinking":false}'
PATH_ENV="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

# name -> working directory, then program arguments
spec() {
  case "$1" in
    fm) echo "$HOME"; echo /usr/bin/fm serve --port 18099 ;;
    qwen) echo "$HOME"; echo "$HOME/.local/bin/mlx_lm.server" --model mlx-community/Qwen3-4B-Instruct-2507-4bit --host 127.0.0.1 --port 18101 ;;
    qwen-large) echo "$HOME"; echo "$HOME/.local/bin/mlx_lm.server" --model mlx-community/Qwen3.8-27B-4bit --host 127.0.0.1 --port 18103 --max-tokens 4096 --chat-template-args "$NO_THINKING" ;;
    embed) echo "$REPO/agent-runtime/embed"; echo "$HOME/.local/bin/uv" run --with "sentence-transformers[image]" --with torch server.py --port 18102 ;;
    runtime) echo "$REPO/agent-runtime"; echo "$REPO/agent-runtime/target/release/agent-runtime" --state-dir "$HOME/.holon-agents" --listen 127.0.0.1:18017 --local-url http://127.0.0.1:18099 --stt-bin "$HOME/.holon-agents/holon-stt" --embed-url http://127.0.0.1:18102 --embed-service io.holon.embed ;;
    bridge) echo "$REPO/agent-matrix"; echo "$REPO/agent-matrix/target/release/agent-matrix" run --config "$HOME/.holon-matrix/bridge.json" ;;
  esac
}

plist() {
  local name="$1" dir; local -a args
  { read -r dir; read -r -a args; } < <(spec "$name")
  # arguments may contain spaces-free paths only; one <string> each
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
  echo '<plist version="1.0"><dict>'
  echo "<key>Label</key><string>io.holon.$name</string>"
  echo "<key>ProgramArguments</key><array>"
  for a in "${args[@]}"; do echo "  <string>$a</string>"; done
  echo "</array>"
  echo "<key>WorkingDirectory</key><string>$dir</string>"
  echo "<key>EnvironmentVariables</key><dict><key>PATH</key><string>$PATH_ENV</string>"
  echo "  <key>HOLON_EMBED_URL</key><string>http://127.0.0.1:18102</string>"
  # a PATH to the key file, never the key: Jev routes messages when it exists
  [ -f "$HOME/.comp-secrets/typesafe" ] && echo "  <key>HOLON_JEV_KEY_FILE</key><string>$HOME/.comp-secrets/typesafe</string>"
  echo "</dict>"
  if [[ "$LAZY" == *" $name "* ]]; then
    echo "<key>RunAtLoad</key><false/><key>KeepAlive</key><false/>"
  else
    echo "<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>"
  fi
  echo "<key>ThrottleInterval</key><integer>10</integer>"
  echo "<key>StandardOutPath</key><string>$LOGS/$name.log</string>"
  echo "<key>StandardErrorPath</key><string>$LOGS/$name.log</string>"
  echo "</dict></plist>"
}

case "${1:-}" in
  install)
    mkdir -p "$AGENTS" "$LOGS"
    for b in "$REPO/agent-runtime/target/release/agent-runtime" "$REPO/agent-matrix/target/release/agent-matrix"; do
      [ -x "$b" ] || { echo "missing $b — cargo build --release there first"; exit 1; }
    done
    # A binary cargo rewrote in place can be killed on launch (OS_REASON_CODESIGNING) until it is re-signed.
    for b in "$REPO/agent-runtime/target/release/agent-runtime" "$REPO/agent-matrix/target/release/agent-matrix"; do
      codesign --force --sign - "$b" 2>/dev/null || true
    done
    for s in "${SERVICES[@]}"; do
      f="$AGENTS/io.holon.$s.plist"
      launchctl bootout "gui/$UID_/io.holon.$s" 2>/dev/null || true
      # bootout returns before the job is gone; bootstrapping too soon fails with an I/O error
      for _ in $(seq 1 30); do launchctl print "gui/$UID_/io.holon.$s" >/dev/null 2>&1 || break; sleep 1; done
      plist "$s" > "$f"
      plutil -lint "$f" >/dev/null
      launchctl bootstrap "gui/$UID_" "$f"
      echo "started io.holon.$s"
    done ;;
  uninstall)
    for s in "${SERVICES[@]}"; do
      launchctl bootout "gui/$UID_/io.holon.$s" 2>/dev/null || true
      rm -f "$AGENTS/io.holon.$s.plist"
      echo "removed io.holon.$s"
    done ;;
  status)
    for s in "${SERVICES[@]}"; do
      printf '%-8s ' "$s"
      launchctl print "gui/$UID_/io.holon.$s" 2>/dev/null | awk '/state =|pid =|last exit/ {printf "%s ", $0}' || true
      echo
    done ;;
  *) sed -n 2,9p "$0"; exit 1 ;;
esac
