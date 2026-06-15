// In-process benchmark: the components running via jco IN this Node process —
// the raw compute cost of each op, no network, no host. Reuses the transpiled
// `gen/` from the jco-embed (auth) and jco-cache (cache) examples.
//
// Run their transpile first (the runner script does it):
//   (cd ../examples/jco-embed && npm run transpile)
//   (cd ../examples/jco-cache  && npm run transpile)

import { writeFileSync } from "node:fs";
import { measure, type Result } from "./measure.js";

// auth-guard (composed: includes rate-limiter) — exported interfaces.
import { accounts, authorizer, session } from "../../examples/jco-embed/gen/auth_guard.js";
// cache:store
import { cache } from "../../examples/jco-cache/gen/cache.js";
// idempotency:guard/store
import { store as idem } from "../../examples/jco-idempotency/gen/idempotency_guard.js";
// featureflags:guard/evaluator
import { evaluator as flags } from "../../examples/jco-featureflags/gen/feature_flags.js";
// audit:log/recorder
import { recorder as audit } from "../../examples/jco-audit/gen/audit_log.js";
// webhook:ingest/verifier (composed with idempotency-guard)
import { verifier as webhook } from "../../examples/jco-webhook/gen/webhook_ingest.js";
import { __seed as seedWebhookKv } from "../../examples/jco-webhook/src/keyvalue-shim.js";
import { createHmac } from "node:crypto";

const enc = (s: string) => new TextEncoder().encode(s);

async function main() {
  const results: Result[] = [];
  const tenant = "bench";
  const password = "hunter2hunter";

  // --- accounts/session/authorizer ---

  // register: throwaway unique emails (write path: argon2 hash + kv set)
  let n = 0;
  results.push(
    await measure("register", () => accounts.register(`u${n++}@b.com`, password, tenant), {
      iters: 300,
    }),
  );

  // a fixed user for the read/verify paths
  accounts.register("bench@b.com", password, tenant);
  results.push(
    await measure("login", () => accounts.login("bench@b.com", password, tenant), { iters: 300 }),
  );

  const tok = (accounts.login("bench@b.com", password, tenant) as { accessToken: string })
    .accessToken;
  results.push(await measure("introspect", () => authorizer.introspect(tok), { iters: 5000 }));
  results.push(
    await measure(
      "authorize",
      () => {
        try {
          authorizer.authorize(tok, { target: "demo", action: "read" });
        } catch {
          /* 403 throws — still exercises the full path */
        }
      },
      { iters: 5000 },
    ),
  );
  results.push(await measure("session.lookup", () => session.lookup(tok), { iters: 5000 }));

  // --- cache ---
  cache.set("k", enc("v"), 60n);
  results.push(await measure("cache.get(hit)", () => cache.get("k"), { iters: 20000 }));
  results.push(await measure("cache.set", () => cache.set("k", enc("v"), 60n), { iters: 20000 }));
  results.push(await measure("cache.get(miss)", () => cache.get("absent"), { iters: 20000 }));

  // --- idempotency:guard ---
  // begin(miss): unique key each iter -> reserves a fresh pending record (write).
  let m = 0;
  results.push(
    await measure("idem.begin(miss)", () => idem.begin(`miss-${m++}`, 3600n), { iters: 20000 }),
  );
  // a completed key for the replay path.
  idem.begin("hit", 3600n);
  idem.complete("hit", 200, enc('{"ok":true}'));
  results.push(await measure("idem.begin(replay)", () => idem.begin("hit", 3600n), { iters: 20000 }));
  results.push(
    await measure("idem.complete", () => idem.complete("done", 200, enc('{"ok":true}')), {
      iters: 20000,
    }),
  );

  // --- featureflags:guard ---
  const ctx = { tenant: "bench", subject: "user-42" };
  results.push(
    await measure("flags.isEnabled(bool)", () => flags.isEnabled("new-checkout", ctx), {
      iters: 20000,
    }),
  );
  results.push(
    await measure("flags.isEnabled(rollout)", () => flags.isEnabled("beta-search", ctx), {
      iters: 20000,
    }),
  );
  results.push(
    await measure(
      "flags.setRule",
      () => flags.setRule("dark-mode", "bench", { tag: "enabled" }),
      { iters: 20000 },
    ),
  );
  // list-flags walks the keyspace — seed a handful of rules first.
  for (let i = 0; i < 8; i++) flags.setRule(`seeded-${i}`, "bench", { tag: "disabled" });
  results.push(await measure("flags.listFlags", () => flags.listFlags("bench"), { iters: 5000 }));

  // --- audit:log ---
  // record-event: kv write + stderr echo (suppress the echo noise via fd later;
  // here it's the compute+write cost).
  results.push(
    await measure(
      "audit.record-event",
      () =>
        audit.recordEvent({
          id: "",
          traceId: "",
          spanId: "",
          timestamp: 0n,
          event: "authorize",
          outcome: "allow",
          tenant: "bench",
          subject: "u1",
          detail: "orders:read",
        }),
      { iters: 5000 },
    ),
  );

  // --- webhook:ingest (composed with idempotency-guard) ---
  const SECRET = "whsec_bench";
  seedWebhookKv("wh-secret", SECRET);
  const wbody = enc('{"event":"ping"}');
  const wsig = createHmac("sha256", SECRET).update(wbody).digest("hex");
  // unique delivery-id each iter -> exercises the full verify + dedup-miss path.
  let w = 0;
  results.push(
    await measure(
      "webhook.ingest(verify+dedup)",
      () => webhook.ingest(wbody, wsig, "wh-secret", `evt-${w++}`),
      { iters: 5000 },
    ),
  );

  const out = { kind: "in-process", node: process.version, when: Date.now(), results };
  writeFileSync(new URL("../results-inproc.json", import.meta.url), JSON.stringify(out, null, 2));

  console.table(
    results.map((r) => ({
      op: r.op,
      "mean µs": (r.meanNs / 1000).toFixed(2),
      "p99 µs": (r.p99Ns / 1000).toFixed(2),
      "ops/sec": r.opsPerSec.toLocaleString(),
    })),
  );
}

main();
