#!/usr/bin/env bash
# Run the gate rate limiter as a REAL Golem agent and prove EXACT serialization:
# fire N concurrent `take`s at one key and exactly `capacity` succeed — because a
# Golem worker is a single-threaded durable actor per key (no CAS, no over-admit).
# Contrast: the shared-store gate-domain over-admits under the same load.
#
# Reuses the Golem 1.5 binary vendored for the golem-workflow provider e2e.
set -euo pipefail
cd "$(dirname "$0")/golem"
G="../../../providers/golem-workflow/.bin/golem"

if [ ! -x "$G" ]; then
  echo "Golem binary not found — run \`just golem-e2e\` once to fetch it." >&2
  exit 1
fi

# start the local Golem server if the gateway (:9006) isn't already up.
if ! python3 -c "import urllib.request as u; u.urlopen('http://127.0.0.1:9006/', timeout=2)" 2>/dev/null; then
  echo "starting golem server..."; "$G" server run --clean >/tmp/golem-server.log 2>&1 &
  for _ in $(seq 1 60); do lsof -nP -iTCP:9006 -sTCP:LISTEN >/dev/null 2>&1 && break; sleep 2; done
fi

echo "building + deploying the gate agent..."
"$G" deploy -Y 2>&1 | tail -3

echo "=== exact serialization: 24 concurrent takes at one key, capacity 10 ==="
python3 - <<'PY'
import urllib.request as u, threading, json
def post(key, path):
    r = u.Request(f"http://127.0.0.1:9006/gate/{key}{path}", method="POST", headers={"Host": "gate.localhost:9006"})
    return u.urlopen(r, timeout=12).read().decode()
def allowed(resp):
    return json.loads(json.loads(resp))["allowed"]
ok = True
for trial in range(3):
    key = f"golem{trial}"
    post(key, "/reset")  # -> capacity 10
    res = [None] * 24
    def hit(i, key=key): res[i] = allowed(post(key, "/take"))
    ts = [threading.Thread(target=hit, args=(i,)) for i in range(24)]
    [t.start() for t in ts]; [t.join() for t in ts]
    a = sum(1 for x in res if x)
    exact = a == 10
    ok = ok and exact
    print(f"  trial {trial}: {a}/24 admitted (capacity 10) -> {'EXACT' if exact else 'NOT EXACT'}")
print("\nGolem worker = single-writer per key -> exact. (gate-domain's shared-store CAS over-admits.)")
raise SystemExit(0 if ok else 1)
PY
