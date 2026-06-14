// E2E: verify a REAL Ory Hydra JWT in-process against Hydra's live JWKS.
// Skips cleanly if Hydra isn't reachable (CI without the IdP up).
//
// Prereq: `docker compose --profile ory up -d` with STRATEGIES_ACCESS_TOKEN=jwt.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";

const ADMIN = process.env.HYDRA_ADMIN ?? "http://localhost:4445";
const PUBLIC = process.env.HYDRA_PUBLIC ?? "http://localhost:4444";
process.env.EXPECTED_ISSUER ??= PUBLIC;
process.env.ALLOWED_ALGS ??= "RS256,ES256";
process.env.AUDIT_ENABLED ??= "false";

async function hydraUp(): Promise<boolean> {
  try {
    const r = await fetch(`${PUBLIC}/.well-known/openid-configuration`, {
      signal: AbortSignal.timeout(2000),
    });
    return r.ok;
  } catch {
    return false;
  }
}

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

const up = await hydraUp();

describe("Ory Hydra JWT verification (in-process)", { skip: up ? false : `Hydra unreachable at ${PUBLIC}` }, () => {
  let token: string;
  let authorizer: typeof import("../gen/auth_guard.js").authorizer;

  before(async () => {
    token = await mintJwt();
    ({ authorizer } = await import("../gen/auth_guard.js"));
  });

  it("mints a real RS256 JWT (3 segments)", () => {
    assert.equal(token.split(".").length, 3);
    const h = JSON.parse(Buffer.from(token.split(".")[0], "base64url").toString());
    assert.equal(h.alg, "RS256");
  });

  it("verifies it against Hydra's live JWKS and returns a principal", () => {
    const p = authorizer.introspect(token) as { subject: string; scopes: string[] };
    assert.ok(p.subject.length > 0, "has a subject");
    assert.ok(p.scopes.includes("openid"), "carries the openid scope");
  });

  it("rejects a tampered token (signature mismatch -> 401)", () => {
    // flip a char in the signature segment
    const [h, b, s] = token.split(".");
    const bad = `${h}.${b}.${s.slice(0, -2)}${s.endsWith("AA") ? "BB" : "AA"}`;
    assert.throws(() => authorizer.introspect(bad));
  });
});
