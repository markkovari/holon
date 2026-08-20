# support:desk's gates, named. Everything they do lives in `components/gate-lib.sh`.
GATE_CRATE=support-desk-domain
GATE_APP=support
# ratelimit:guard and audit:log are auth-guard's imports, not this app's, and the
# composition needs them built regardless.
GATE_PKGS="-p support-desk-domain -p auth-guard -p rate-limiter -p audit-log \
-p session-store -p quota -p outbox -p notify-dispatch -p ai-inference \
-p anthropic-provider -p record-store"

# shellcheck source=components/gate-lib.sh
. components/gate-lib.sh

desk_requires_auth() {
  gate_requires_capability "auth:identity/authorizer" \
    "resolving a bearer token is a solved problem in this repository and \`authorize\` does the \
verification and the permission check in one call — parsing a token by hand is how this part fails"
}

# --- a sink that can be broken on purpose ------------------------------------------
#
# Delivery is the whole subject of this app and none of it is observable against a far end
# that always works: an app that sends inline, one that acks a refusal, and one that retries
# something already delivered all look identical on the happy path. So the gate runs its own
# webhook receiver, logs every request to a file, and answers 500 whenever a flag file
# exists — which is how "the far end refused" becomes a thing a test can arrange.
#
# `python3 -m http.server` cannot do this: it has no way to fail on demand and no way to
# record bodies. Twenty lines of `http.server` can do both.
sink_start() {
  SINK_PORT=$(( 30000 + RANDOM % 20000 ))
  SINK_LOG="$(mktemp -t gate-sink-XXXX)"
  SINK_FAIL="$(mktemp -t gate-sinkfail-XXXX)"
  rm -f "$SINK_FAIL"
  SINK_URL="http://127.0.0.1:$SINK_PORT/hook"
  SINK_PY="$(mktemp -t gate-sink-py-XXXX)"
  cat > "$SINK_PY" <<'PY'
import http.server, json, os, sys, threading

port, log_path, fail_path = int(sys.argv[1]), sys.argv[2], sys.argv[3]
lock = threading.Lock()


class Sink(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length).decode("utf-8", "replace")
        # Every arrival is recorded, refused or not — "how many times did this arrive" is
        # the question at-least-once is about, and a refusal still arrived.
        failing = os.path.exists(fail_path)
        with lock:
            with open(log_path, "a") as f:
                f.write(json.dumps({"path": self.path, "body": raw, "refused": failing}) + "\n")
        self.send_response(500 if failing else 200)
        self.end_headers()
        self.wfile.write(b'{"ok":true}')

    def log_message(self, *_):
        pass


http.server.HTTPServer(("127.0.0.1", port), Sink).serve_forever()
PY
  python3 "$SINK_PY" "$SINK_PORT" "$SINK_LOG" "$SINK_FAIL" &
  SINK_PID=$!
  # Off the job table: killing it in the trap would otherwise print `Terminated: 15` on a
  # PASSING run, and that lands in the branch's feedback looking like the failure it is not.
  disown "$SINK_PID" 2>/dev/null || true
  local _
  for _ in $(seq 1 40); do
    curl -s -o /dev/null --max-time 1 -X POST -d '{}' "$SINK_URL" && break
    sleep 0.25
  done
  # The probe above is itself a delivery, so the log starts clean afterwards.
  : > "$SINK_LOG"
  # The component reaches it over `wasi:http`, which is default-deny by name and refuses
  # loopback twice over.
  GATE_EGRESS="${GATE_EGRESS:-} --egress 127.0.0.1:$SINK_PORT"
  GATE_PRIVATE_EGRESS=--allow-private-egress
}

sink_break() { : > "$SINK_FAIL"; }
sink_repair() { rm -f "$SINK_FAIL"; }
# `grep -c .` prints 0 AND exits 1 on an empty file, so the `|| echo 0` idiom prints TWO
# zeroes and every "must be 0" comparison fails against a string with a newline in it — the
# empty case, which is exactly the one that means "nothing was sent".
sink_deliveries() {
  [ -f "$SINK_LOG" ] || { echo 0; return; }
  awk 'END { print NR }' "$SINK_LOG"
}
sink_stop() {
  [ -n "${SINK_PID:-}" ] && kill "$SINK_PID" 2>/dev/null
  rm -f "$SINK_LOG" "$SINK_FAIL" "${SINK_PY:-}"
}
