"""Shared HTTP-behavior test harness for held-out app goals.

Not the specification (the goal's own text in `.comp/goals/*.toml` is), but the
judge: real HTTP requests against a running, composed instance, exactly the
same discipline `bytes-codec/gate.sh` and `moderation-domain`'s CONTRACT tests
use — a gate that only checked `cargo component check` would pass code that
does nothing.

Usage: python3 domain-gate.py <base-url> <role-scopes-json> <scenario.py path>
"""
import importlib.util
import json
import sys
import urllib.request
import urllib.error

BASE = sys.argv[1]
ROLE_SCOPES = json.loads(sys.argv[2])
SCENARIO_PATH = sys.argv[3]
# BASE is a fixed http://127.0.0.1:<port> the CALLING gate.sh hard-codes (never
# request data), so the scheme is pinned here rather than trusted implicitly —
# the thing a urllib audit actually cares about (no file://, no surprise host).
if not BASE.startswith("http://127.0.0.1:") and not BASE.startswith("https://127.0.0.1:"):
    raise SystemExit(f"refusing non-loopback base url: {BASE!r}")


def api(method, path, data=None, token=None):
    headers = {}
    if token:
        headers["Authorization"] = "Bearer " + token["token"]
    body = json.dumps(data).encode() if data is not None else None
    if body:
        headers["content-type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=body, headers=headers, method=method)
    try:
        # BASE is scheme-checked above (loopback http/https only) and path/data
        # come from this gate's own scenario code, never external input.
        r = urllib.request.urlopen(req, timeout=10)  # nosemgrep: dynamic-urllib-use-detected
        b = r.read()
        return r.status, (json.loads(b) if b else None)
    except urllib.error.HTTPError as e:
        b = e.read()
        return e.code, (json.loads(b) if b else None)


def login(subject, _password_unused, roles):
    scopes = []
    for r in roles:
        prefix = r.split(":", 1)[0]
        scopes.extend(ROLE_SCOPES.get(r, ROLE_SCOPES.get(prefix, [])))
    code, body = api("POST", "/test/token", {"subject": subject, "roles": roles, "scopes": scopes})
    assert code == 201, ("mint token", code, body)
    return {"token": body["token"], "subject": body["subject"]}


if __name__ == "__main__":
    spec = importlib.util.spec_from_file_location("scenario", SCENARIO_PATH)
    scenario = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scenario)
    scenario.run(api, login)
    print("PASS: every scenario assertion held")
