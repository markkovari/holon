// IdP JWT verification benchmark (in-process). Mints a REAL Ory Hydra RS256 JWT,
// primes the JWKS cache (first introspect fetches Hydra's JWKS over wasi:http),
// then times warm verifications — the per-request cost of validating an
// external-IdP token against cached keys.
//
// Needs Hydra up (compose --profile ory, STRATEGIES_ACCESS_TOKEN=jwt) and the
// idp-oidc example transpiled (gen/). Appends to results-inproc.json.

import { readFileSync, writeFileSync } from "node:fs";
import { measure, type Result } from "./measure.js";
import { authorizer } from "../../examples/idp-oidc/gen/auth_guard.js";

const ADMIN = process.env.HYDRA_ADMIN ?? "http://localhost:4445";
const PUBLIC = process.env.HYDRA_PUBLIC ?? "http://localhost:4444";
process.env.EXPECTED_ISSUER ??= PUBLIC;
process.env.ALLOWED_ALGS ??= "RS256,ES256";
process.env.AUDIT_ENABLED ??= "false";

async function mintJwt(): Promise<string> {
  const reg = await fetch(`${ADMIN}/admin/clients`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      grant_types: ["client_credentials"],
      token_endpoint_auth_method: "client_secret_post",
      scope: "openid offline",
    }),
  });
  const c = (await reg.json()) as { client_id: string; client_secret: string };
  const t = await fetch(`${PUBLIC}/oauth2/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "client_credentials",
      client_id: c.client_id,
      client_secret: c.client_secret,
      scope: "openid",
    }),
  });
  return ((await t.json()) as { access_token: string }).access_token;
}

async function main() {
  const token = await mintJwt();
  if (token.split(".").length !== 3) {
    console.error("Hydra returned an opaque token — set STRATEGIES_ACCESS_TOKEN=jwt.");
    process.exit(1);
  }

  // prime: first introspect fetches + caches Hydra's JWKS (one network hit).
  authorizer.introspect(token);

  const results: Result[] = [];
  results.push(
    await measure("idp.introspect(RS256, warm JWKS)", () => authorizer.introspect(token), {
      iters: 3000,
    }),
  );

  // merge into the in-process results so the plot shows it alongside the rest
  const path = new URL("../results-inproc.json", import.meta.url);
  let doc: { kind: string; node: string; when: number; results: Result[] };
  try {
    doc = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    doc = { kind: "in-process", node: process.version, when: Date.now(), results: [] };
  }
  // replace any prior idp row, then append
  doc.results = doc.results.filter((r) => !r.op.startsWith("idp.")).concat(results);
  writeFileSync(path, JSON.stringify(doc, null, 2));

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
