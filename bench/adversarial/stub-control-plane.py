"""Stand-in control plane serving N apps, so ADR-0023's two tenants can land on
one host. The thing under test is the host, not the control plane."""
import json, sys, hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer

MANIFESTS = json.load(open(sys.argv[1]))
ARTIFACTS = json.loads(sys.argv[2])          # {"component-id": "path/to.wasm"}
PORT = int(sys.argv[3])
SECRET = "test-secret"

pushed = {}                                   # component id -> digest
RAW = {k: open(v, "rb").read() for k, v in ARTIFACTS.items()}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _auth(self):
        if self.headers.get("x-platform-secret") != SECRET:
            self.send_response(401); self.end_headers(); return False
        return True

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if not self._auth():
            return
        if self.path.startswith("/api/internal/revisions"):
            out = []
            for i, m in enumerate(MANIFESTS):
                m = json.loads(json.dumps(m))
                ok = True
                for c in m["components"]:
                    c["digest"] = pushed.get(c["id"], "")
                    ok = ok and bool(c["digest"])
                if ok:
                    out.append({"id": f"r{i}", "manifest": m})
            self._json({"revisions": out})
        elif self.path.startswith("/api/internal/pending-pushes"):
            self._json({"pending": [
                {"key": k, "repo": k, "exports": [], "imports": []}
                for k in RAW if k not in pushed
            ]})
        elif self.path.startswith("/api/internal/artifact"):
            key = self.path.split("key=")[-1]
            raw = RAW.get(key, b"")
            self.send_response(200 if raw else 404)
            self.send_header("content-type", "application/wasm")
            self.send_header("content-length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        else:
            self.send_response(404); self.end_headers()

    def do_POST(self):
        if not self._auth():
            return
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        if self.path.startswith("/api/internal/pushed"):
            pushed[body["key"]] = body["digest"]
            print(f"platform: {body['key']} -> {body['digest'][:19]}…", flush=True)
            self._json({"ok": True})
        elif self.path.startswith("/api/internal/status"):
            for u in body.get("unschedulable", []):
                print(f"platform: UNSCHEDULABLE {u['tenant']}/{u['app']}: {u['reason']}", flush=True)
            self._json({"ok": True})
        else:
            self.send_response(404); self.end_headers()


print(f"platform: {len(MANIFESTS)} app(s), {len(RAW)} artifact(s) on :{PORT}", flush=True)
HTTPServer(("127.0.0.1", PORT), H).serve_forever()
