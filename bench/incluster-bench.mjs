// In-cluster HTTP bench: runs inside a pod, hits the host pod IP directly, so
// there is no flaky port-forward. Prints a single JSON line (RESULTS:{...}) the
// outer script greps from pod logs. TARGET env = http://<host-pod-ip>:8001.

const BASE = process.env.TARGET;
const ITERS = Number(process.env.ITERS ?? 120);

const samples = (n) => new Float64Array(n);
function stats(op, arr) {
  const s = Array.from(arr).sort((a, b) => a - b);
  const mean = s.reduce((x, y) => x + y, 0) / s.length;
  const pct = (p) => s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
  return {
    op,
    iters: s.length,
    meanNs: Math.round(mean),
    p50Ns: Math.round(pct(50)),
    p95Ns: Math.round(pct(95)),
    p99Ns: Math.round(pct(99)),
    opsPerSec: Math.round(1e9 / mean),
  };
}

async function timeOp(op, fn, iters, warmup = 5) {
  for (let i = 0; i < warmup; i++) await fn();
  const xs = samples(iters);
  for (let i = 0; i < iters; i++) {
    const t0 = process.hrtime.bigint();
    await fn();
    xs[i] = Number(process.hrtime.bigint() - t0);
  }
  return stats(op, xs);
}

const J = { "content-type": "application/json" };
const post = (p, b, t) =>
  fetch(`${BASE}${p}`, {
    method: "POST",
    headers: t ? { ...J, authorization: `Bearer ${t}` } : J,
    body: JSON.stringify(b),
  }).then((r) => r.arrayBuffer().then(() => r));
const get = (p, t) =>
  fetch(`${BASE}${p}`, { headers: { authorization: `Bearer ${t}` } }).then((r) =>
    r.arrayBuffer().then(() => r),
  );

const tenant = "httpbench";
const password = "hunter2hunter";
const results = [];

let n = 0;
results.push(await timeOp("POST /register", () => post("/register", { email: `h${n++}@b.com`, password, tenant }), Math.min(60, ITERS)));
await post("/register", { email: "hb@b.com", password, tenant });
results.push(await timeOp("POST /login", () => post("/login", { email: "hb@b.com", password, tenant }), ITERS));
const tok = await post("/login", { email: "hb@b.com", password, tenant })
  .then((r) => r.json?.())
  .catch(() => null);
const token = await fetch(`${BASE}/login`, { method: "POST", headers: J, body: JSON.stringify({ email: "hb@b.com", password, tenant }) })
  .then((r) => r.json())
  .then((j) => j.access_token);
void tok;
results.push(await timeOp("GET /me", () => get("/me", token), ITERS));
results.push(await timeOp("POST /verify", () => post("/verify", { target: "demo", action: "read" }, token), ITERS));

console.log("RESULTS:" + JSON.stringify({ kind: "http", base: BASE, node: process.version, when: Date.now(), results }));
