// HTTP roundtrip benchmark: drive the DEPLOYED accounts-app over HTTP. This is
// the full path — Node fetch -> port-forward -> wasmCloud http-server provider
// -> wrpc -> component -> kv (NATS). Compare against the in-process numbers to
// see the transport + host overhead.
//
// Prereq: a single wasmCloud host with accounts-app, port-forwarded to :8001
// (see comp/README + `just k8s-collapse`). Override with AUTH_BASE_URL.

import { writeFileSync } from "node:fs";
import { measure, type Result } from "./measure.js";

const BASE = process.env.AUTH_BASE_URL ?? "http://localhost:8001";

// Retry transient connection drops — the local port-forward can flap while the
// operator reconciles host pods; that is infra noise, not the component's
// latency. We retry the fetch (not counted as extra iterations by `measure`,
// which times the whole call including any retry — acceptable: drops are rare).
async function withRetry<T>(fn: () => Promise<T>, tries = 5): Promise<T> {
  let last: unknown;
  for (let i = 0; i < tries; i++) {
    try {
      return await fn();
    } catch (e) {
      last = e;
      await new Promise((r) => setTimeout(r, 200 * (i + 1)));
    }
  }
  throw last;
}

async function post(path: string, body: unknown, token?: string) {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const res = await withRetry(() =>
    fetch(`${BASE}${path}`, { method: "POST", headers, body: JSON.stringify(body) }),
  );
  await res.arrayBuffer(); // drain
  return res;
}
async function get(path: string, token: string) {
  const res = await withRetry(() =>
    fetch(`${BASE}${path}`, { headers: { authorization: `Bearer ${token}` } }),
  );
  await res.arrayBuffer();
  return res;
}

async function main() {
  const tenant = "httpbench";
  const password = "hunter2hunter";
  const results: Result[] = [];

  // HTTP iters are lower than in-process — these are real network roundtrips.
  // register is argon2-bound (~30ms each), so it gets fewer iters.
  const iters = Number(process.env.HTTP_ITERS ?? 150);

  let n = 0;
  results.push(
    await measure("POST /register", () => post("/register", { email: `h${n++}@b.com`, password, tenant }), {
      iters: Math.min(60, iters),
      warmup: 5,
    }),
  );

  await post("/register", { email: "hb@b.com", password, tenant });
  results.push(
    await measure("POST /login", () => post("/login", { email: "hb@b.com", password, tenant }), { iters }),
  );

  // grab a session token for the guarded endpoints
  const tokRes = await fetch(`${BASE}/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: "hb@b.com", password, tenant }),
  });
  const tok = ((await tokRes.json()) as { access_token: string }).access_token;

  results.push(await measure("GET /me", () => get("/me", tok), { iters }));
  results.push(
    await measure("POST /verify", () => post("/verify", { target: "demo", action: "read" }, tok), { iters }),
  );

  const out = { kind: "http", base: BASE, node: process.version, when: Date.now(), results };
  writeFileSync(new URL("../results-http.json", import.meta.url), JSON.stringify(out, null, 2));

  console.table(
    results.map((r) => ({
      op: r.op,
      "mean ms": (r.meanNs / 1e6).toFixed(3),
      "p95 ms": (r.p95Ns / 1e6).toFixed(3),
      "p99 ms": (r.p99Ns / 1e6).toFixed(3),
      "req/sec": r.opsPerSec.toLocaleString(),
    })),
  );
}

main();
