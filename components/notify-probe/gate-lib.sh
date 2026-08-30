# The notify gates, and the two real things they need running.
#
# `mail:send` is delivered over HTTP because `comp-host` wires no `wasi:sockets` —
# a component cannot open a TCP connection, so it cannot speak SMTP. MailHog ingests
# SMTP only. `comp-mailrelay` is the bridge: it accepts the same JSON body the
# component POSTs to Resend and turns it into a real SMTP session.
#
# Both are started HERE rather than assumed. A gate that needs `docker compose up`
# first fails on a clean machine as "your email code is broken", which is exactly
# the class of harness lie this repository keeps paying for.

# A port nothing is listening on, checked rather than hoped for.
#
# `RANDOM % 20000` alone COLLIDES, and it is not rare: CI runs 31 of these gates
# beside 21 Rust suites and a fleet, and every one of them wants a port. What a
# collision looks like from the outside is `Address already in use (os error 98)`
# — a gate that failed for a reason with nothing to do with the code it grades.
#
# `/dev/tcp` is bash's own, so this needs nothing installed. A successful connect
# means something is already there; a refused one means the port is free.
gate_free_port() {
  local p i
  for i in $(seq 1 50); do
    p=$(( 20000 + RANDOM % 40000 ))
    if ! (exec 3<>/dev/tcp/127.0.0.1/"$p") 2>/dev/null; then
      echo "$p"; return 0
    fi
    exec 3<&- 2>/dev/null || true
  done
  # Fifty taken ports means something is wrong that a fifty-first will not fix.
  echo "gate: no free port after 50 tries" >&2
  return 1
}

GATE_CRATE=notify-probe
GATE_APP=notify
GATE_PKGS="-p notify-probe -p notify-prefs -p notify-inbox -p mail-http -p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

MAILHOG_BIN="${MAILHOG_BIN:-$HOME/go/bin/MailHog}"

# Start MailHog and the relay, on ports nothing else is using, and point the
# component's gateway config at the relay.
notify_start_mail() {
  [ -x "$MAILHOG_BIN" ] || {
    echo "no MailHog at '$MAILHOG_BIN' — the gate cannot prove an email was delivered."
    echo "  go install github.com/mailhog/MailHog@latest"
    exit 1
  }
  SMTP_PORT=$(gate_free_port)
  MAIL_API_PORT=$(gate_free_port)
  RELAY_PORT=$(gate_free_port)
  MAIL_API="http://127.0.0.1:$MAIL_API_PORT"

  "$MAILHOG_BIN" -smtp-bind-addr "127.0.0.1:$SMTP_PORT" \
    -api-bind-addr "127.0.0.1:$MAIL_API_PORT" \
    -ui-bind-addr "127.0.0.1:$MAIL_API_PORT" >/dev/null 2>&1 &
  MAILHOG_PID=$!
  disown "$MAILHOG_PID" 2>/dev/null || true

  RELAY_BIN="${COMP_MAILRELAY:-reconciler/target/release/comp-mailrelay}"
  [ -x "$RELAY_BIN" ] || {
    echo "no comp-mailrelay at '$RELAY_BIN' — cargo build --release --bin comp-mailrelay"
    exit 1
  }
  "$RELAY_BIN" "127.0.0.1:$RELAY_PORT" "127.0.0.1:$SMTP_PORT" >/dev/null 2>&1 &
  RELAY_PID=$!
  disown "$RELAY_PID" 2>/dev/null || true

  local _
  for _ in $(seq 1 40); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$MAIL_API/api/v2/messages")" = "200" ] \
      && [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "http://127.0.0.1:$RELAY_PORT/")" = "200" ] \
      && break
    sleep 0.25
  done
  [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$MAIL_API/api/v2/messages")" = "200" ] || {
    echo "MailHog never answered on $MAIL_API — the gate cannot read what was delivered"
    exit 1
  }

  GATE_CONFIG="${GATE_CONFIG:-} --config mail:gateway-url=http://127.0.0.1:$RELAY_PORT/ --config mail:from=events@holon.test"
  GATE_EGRESS="${GATE_EGRESS:-} --egress 127.0.0.1:$RELAY_PORT"
  GATE_PRIVATE_EGRESS=--allow-private-egress
  export MAIL_API
}

notify_stop_mail() {
  [ -n "${MAILHOG_PID:-}" ] && kill "$MAILHOG_PID" 2>/dev/null
  [ -n "${RELAY_PID:-}" ] && kill "$RELAY_PID" 2>/dev/null
  return 0
}

# Everything MailHog is holding, as JSON.
mailbox() { curl -s "$MAIL_API/api/v2/messages"; }

# How many messages MailHog holds whose body contains "$1".
mail_count_containing() {
  mailbox | python3 -c "
import sys, json
needle = sys.argv[1]
box = json.load(sys.stdin)
print(sum(1 for m in box.get('items', []) if needle in (m['Content']['Body'] or '')))" "$1"
}

# The first message whose body contains "$1", as 'to|subject|body'.
mail_find() {
  mailbox | python3 -c "
import sys, json
needle = sys.argv[1]
box = json.load(sys.stdin)
for m in box.get('items', []):
    if needle in (m['Content']['Body'] or ''):
        h = m['Content']['Headers']
        print('|'.join([h.get('To',[''])[0], h.get('Subject',[''])[0], (m['Content']['Body'] or '').strip()]))
        break" "$1"
}

# Did `channel` deliver, in this /notify answer?
#
# Parsed, not grepped. serde_json orders a map's keys alphabetically, so an outcome
# serialises as {"channel":…,"detail":…,"ok":…} — and a gate matching
# '"channel":"in-app","ok":true' fails against a component that is working, which is
# the same shape of harness lie as the quoting bug this library already records.
outcome_ok() { # outcome_ok <json> <channel>
  printf '%s' "$1" | python3 -c "
import sys, json
want = sys.argv[1]
d = json.load(sys.stdin)
print('yes' if any(o['channel'] == want and o['ok'] for o in d.get('outcomes', [])) else 'no')" "$2"
}

# Which channels were attempted at all.
outcome_channels() { # outcome_channels <json>
  printf '%s' "$1" | python3 -c "
import sys, json
print(','.join(o['channel'] for o in json.load(sys.stdin).get('outcomes', [])))"
}
