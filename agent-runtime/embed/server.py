"""Local embedding service for the runtime and bridge: EmbeddingGemma 2 over HTTP.

  POST /embed  {"texts": ["..."], "kind": "query" | "document", "dim": 256}
  ->           {"vectors": [[...], ...], "dim": 256}

`query` is for what someone asked, `document` for what is being searched (an agent's
description, a memory): the model is trained with different prompts for the two.
`dim` (768, 512, 256 or 128) shortens the vectors; the full ones are not needed to route.

  uv run --with "sentence-transformers[image]" --with torch server.py --port 18102
"""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from sentence_transformers import SentenceTransformer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, default=18102)
ap.add_argument("--model", default="google/embeddinggemma-2")
args = ap.parse_args()
model = SentenceTransformer(args.model)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/embed":
            return self.reply(404, {"error": "not found"})
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
            texts = [str(t) for t in body["texts"]]
            dim = int(body.get("dim", 256))
            enc = model.encode_query if body.get("kind") == "query" else model.encode_document
            vecs = enc(texts, normalize_embeddings=True, truncate_dim=dim)
            self.reply(200, {"vectors": vecs.tolist(), "dim": dim})
        except Exception as e:  # a bad request must not take the service down
            self.reply(400, {"error": str(e)})

    def do_GET(self):
        self.reply(200, {"status": "ok", "model": args.model})

    def reply(self, code, obj):
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()
