#!/usr/bin/env bash
# Keep the agent stack running across logins and crashes, as launchd user agents.
#
#   ./install.sh install    write the plists and (re)start every service
#   ./install.sh uninstall  stop and remove them
#   ./install.sh status     what is running
#
# Services (label io.holon.<name>): fm (Apple model), qwen (4B model), qwen-large (27B, slower, ~15 GB), embed (EmbeddingGemma 2),
# nats and platform (the vault agents' API keys live in, ADR-0051), vcs (the code store, ADR-0099,
# a component on comp-host, ADR-0134), agentd (coding sessions, ADR-0102, their edits recorded in vcs,
# ADR-0121), runtime (the agents), bridge (Matrix, a component on comp-host, ADR-0135). Logs: ~/Library/Logs/holon/<name>.log. Nothing secret goes in a plist: tokens stay in
# ~/.holon-agents, ~/.holon-matrix and ~/.holon-platform, where the programs read them.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
AGENTS="$HOME/Library/LaunchAgents"
LOGS="$HOME/Library/Logs/holon"
UID_="$(id -u)"
SERVICES=(fm qwen qwen-large embed nats platform vcs agentd runtime bridge)
PLATFORM="$HOME/.holon-platform"
STATE="$HOME/.holon-agents"
MATRIX="$HOME/.holon-matrix"
# Big local models: installed but not started. The runtime starts one when a model chain needs it
# (DeepSeek unavailable or not good enough; or, for embed, a recall or routing call) and stops it
# after 10 idle minutes.
LAZY=" qwen qwen-large embed "
NO_THINKING='{"enable_thinking":false}'
PATH_ENV="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"

# name -> working directory, then program arguments; $MATRIX_EGRESS is one --egress per word.
# shellcheck disable=SC2086
spec() {
  case "$1" in
    fm) echo "$HOME"; echo /usr/bin/fm serve --port 18099 ;;
    qwen) echo "$HOME"; echo "$HOME/.local/bin/mlx_lm.server" --model mlx-community/Qwen3-4B-Instruct-2507-4bit --host 127.0.0.1 --port 18101 ;;
    qwen-large) echo "$HOME"; echo "$HOME/.local/bin/mlx_lm.server" --model mlx-community/Qwen3.8-27B-4bit --host 127.0.0.1 --port 18103 --max-tokens 4096 --chat-template-args "$NO_THINKING" ;;
    embed) echo "$REPO/agent-runtime/embed"; echo "$HOME/.local/bin/uv" run --with "sentence-transformers[image]" --with torch server.py --port 18102 ;;
    nats) echo "$PLATFORM"; echo /opt/homebrew/bin/nats-server -js -sd "$PLATFORM/nats" -a 127.0.0.1 -p 4222 ;;
    platform) echo "$REPO"; echo "$REPO/target/release/comp-host" --component "$REPO/components/target/platform-domain.composed.wasm" --addr 127.0.0.1:3936 --kv nats --nats-url nats://127.0.0.1:4222 --tenant platform --app control-plane --config-file "$PLATFORM/platform.conf" ;;
    vcs) echo "$STATE"; echo "$REPO/target/release/comp-host" --component "$REPO/components/target/vcs_gateway.composed.wasm" --addr 127.0.0.1:18014 --kv sqlite --sqlite-path "$STATE/vcs.db" --dir "$STATE/checkouts" --mem-cap-mb 256 ;;
    agentd) echo "$STATE"; echo "$REPO/target/release/comp-agentd" --addr 127.0.0.1:18016 --token-file "$STATE/agentd-token" --workspace-root "$STATE/workspaces" --state-dir "$STATE/agentd" --provider openai --base-url https://api.deepseek.com --api-key-file "$HOME/.comp-secrets/deepseek" --vcs-url http://127.0.0.1:18014 ;;
    runtime) echo "$REPO/agent-runtime"; echo "$REPO/target/release/agent-runtime" --state-dir "$STATE" --listen 127.0.0.1:18017 --local-url http://127.0.0.1:18099 --stt-bin "$STATE/holon-stt" --embed-url http://127.0.0.1:18102 --embed-service io.holon.embed --platform-file "$STATE/platform.json" --agentd-url http://127.0.0.1:18016 --agentd-token-file "$STATE/agentd-token" --session-model deepseek-chat ;;
    # Synapse dials it over the tailnet. It reaches only what its config names (the homeserver on the
    # tailnet, the runtime on loopback: both private, hence the flag), as `holon matrix egress` lists.
    bridge) echo "$MATRIX"; echo "$REPO/target/release/comp-host" --component "$REPO/components/target/wasm32-wasip2/release/matrix_bridge.wasm" --addr 0.0.0.0:9009 --kv sqlite --sqlite-path "$MATRIX/bridge.db" --config-file "$MATRIX/bridge.conf" $MATRIX_EGRESS --allow-private-egress --pump /internal/pump --pump-every 2 --mem-cap-mb 256 ;;
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

# The platform's secrets, made once: a new master key could not read what the old one sealed.
platform_conf() {
  mkdir -p "$PLATFORM/nats"
  [ -f "$PLATFORM/platform.conf" ] && return
  ( umask 077; printf 'applier-secret = %s\nmaster-key = %s\ningress-suffix = apps.local\n' \
      "$(openssl rand -hex 24)" "$(openssl rand -base64 32)" > "$PLATFORM/platform.conf" )
}

# The token agentd requires of its callers, made once, and where vcs may write a checkout.
tokens() {
  mkdir -p "$STATE/workspaces" "$STATE/checkouts"
  for t in agentd-token; do
    [ -f "$STATE/$t" ] || ( umask 077; openssl rand -hex 24 > "$STATE/$t" )
  done
}

# The runtime's account on the platform, made once, after the platform answers.
platform_account() {
  local f="$HOME/.holon-agents/platform.json" pw applier
  [ -f "$f" ] && return
  for _ in $(seq 1 60); do curl -s -o /dev/null http://127.0.0.1:3936/ && break; sleep 1; done
  pw="$(openssl rand -hex 16)"
  applier="$(sed -n 's/^applier-secret = //p' "$PLATFORM/platform.conf")"
  curl -fs -X POST -H 'content-type: application/json' http://127.0.0.1:3936/api/register \
    -d "{\"email\":\"agents@holon.local\",\"password\":\"$pw\"}" >/dev/null
  mkdir -p "$HOME/.holon-agents"
  ( umask 077; printf '{"url":"http://127.0.0.1:3936","email":"agents@holon.local","password":"%s","applier":"%s"}\n' \
      "$pw" "$applier" > "$f" )
  echo "made the runtime's platform account ($f)"
}

# The bridge component's config file; a native bridge's bridge.json becomes it once, keeping its
# tokens so Synapse's registration stays valid.
matrix_conf() {
  [ -f "$MATRIX/bridge.conf" ] && return 0
  [ -f "$MATRIX/bridge.json" ] || { echo "missing $MATRIX/bridge.conf — holon matrix init first"; exit 1; }
  local jev="$HOME/.comp-secrets/typesafe"
  if [ -f "$jev" ]; then HOLON_JEV_KEY_FILE="$jev" "$REPO/target/release/holon" matrix migrate
  else "$REPO/target/release/holon" matrix migrate; fi
}

# `--egress host:port` for each address the bridge's config names.
matrix_egress() {
  local out a
  out="$("$REPO/target/release/holon" matrix egress)"
  MATRIX_EGRESS=""
  while read -r a; do MATRIX_EGRESS+="--egress $a "; done <<<"$out"
}

# The native bridge's rooms, handed once to the component as soon as it answers.
matrix_state() {
  [ -f "$MATRIX/state.json" ] || return 0
  for _ in $(seq 1 30); do curl -sf http://127.0.0.1:9009/health >/dev/null && break; sleep 1; done
  "$REPO/target/release/holon" matrix import-state
}

case "${1:-}" in
  install)
    mkdir -p "$AGENTS" "$LOGS"
    platform_conf
    tokens
    [ -f "$HOME/.comp-secrets/deepseek" ] || { echo "missing ~/.comp-secrets/deepseek — agentd's model key"; exit 1; }
    for f in "$REPO/target/release/comp-host" "$REPO/components/target/platform-domain.composed.wasm" \
      "$REPO/components/target/vcs_gateway.composed.wasm" "$REPO/components/target/wasm32-wasip2/release/matrix_bridge.wasm"; do
      [ -f "$f" ] || { echo "missing $f — cargo xtask host platform, then cargo xtask build"; exit 1; }
    done
    BINS=(agent-runtime comp-agentd holon)
    for b in "${BINS[@]/#/$REPO/target/release/}"; do
      [ -x "$b" ] || { echo "missing $b — cargo build --release -p $(basename "$b") first"; exit 1; }
    done
    # A binary cargo rewrote in place can be killed on launch (OS_REASON_CODESIGNING) until it is re-signed.
    for b in "${BINS[@]/#/$REPO/target/release/}"; do
      codesign --force --sign - "$b" 2>/dev/null || true
    done
    matrix_conf
    matrix_egress
    for s in "${SERVICES[@]}"; do
      f="$AGENTS/io.holon.$s.plist"
      launchctl bootout "gui/$UID_/io.holon.$s" 2>/dev/null || true
      # bootout returns before the job is gone; bootstrapping too soon fails with an I/O error
      for _ in $(seq 1 30); do launchctl print "gui/$UID_/io.holon.$s" >/dev/null 2>&1 || break; sleep 1; done
      plist "$s" > "$f"
      plutil -lint "$f" >/dev/null
      launchctl bootstrap "gui/$UID_" "$f"
      echo "started io.holon.$s"
      if [ "$s" = platform ]; then platform_account; fi
      if [ "$s" = bridge ]; then matrix_state; fi
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
