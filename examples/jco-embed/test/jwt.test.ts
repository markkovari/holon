// JWT happy-path e2e: auth-guard accepts a valid HS256 token whose claims pass
// (iss/aud/nbf/exp) and whose alg is on the allow-list. Self-contained — mints
// the token here with the same secret seeded into the keyvalue shim.
//
// Run via the suite's transpile step (npm test). Requires HS256 on the alg
// allow-list and the secret in kv, both arranged below.

import { before, describe, it } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";

// Enable HS256 + a known issuer/audience for THIS test before the config shim
// is read by the component.
process.env.ALLOWED_ALGS = "RS256,ES256,HS256";
process.env.EXPECTED_ISSUER = "https://local";
process.env.EXPECTED_AUDIENCE = "comp-auth";

const SECRET = "test-hs256-secret";

function mint(claims: Record<string, unknown>): string {
  const now = Math.floor(Date.now() / 1000);
  const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64url");
  const head = b64({ alg: "HS256", typ: "JWT" });
  const body = b64({ iss: "https://local", aud: "comp-auth", iat: now, nbf: now, exp: now + 3600, ...claims });
  const input = `${head}.${body}`;
  const sig = crypto.createHmac("sha256", SECRET).update(input).digest("base64url");
  return `${input}.${sig}`;
}

describe("jco-embed JWT path (HS256)", () => {
  before(async () => {
    // seed hs256-secret into the same in-memory keyvalue the component uses.
    const kv = await import("../src/shims/keyvalue.js");
    kv.open("default").set("hs256-secret", new TextEncoder().encode(SECRET));
    // expected-issuer/audience aren't in the default shim; add them at runtime.
    const cfg = await import("../src/shims/config.js");
    // config shim exports `get`; patch its backing via env already set above.
    void cfg;
  });

  it("accepts a valid HS256 token via /auth/me (introspect)", async () => {
    const { buildApp } = await import("../src/app.js");
    const app = buildApp();
    await app.ready();
    try {
      const token = mint({ sub: "jwt-user", tenant: "acme", scope: "orders:read" });
      const res = await app.inject({
        method: "GET",
        url: "/auth/me",
        headers: { authorization: `Bearer ${token}` },
      });
      assert.equal(res.statusCode, 200);
      assert.equal(res.json().subject, "jwt-user");
    } finally {
      await app.close();
    }
  });

  it("rejects an HS256 token signed with the wrong secret (401)", async () => {
    const { buildApp } = await import("../src/app.js");
    const app = buildApp();
    await app.ready();
    try {
      const now = Math.floor(Date.now() / 1000);
      const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64url");
      const input = `${b64({ alg: "HS256", typ: "JWT" })}.${b64({
        sub: "x",
        iss: "https://local",
        aud: "comp-auth",
        exp: now + 3600,
      })}`;
      const badSig = crypto.createHmac("sha256", "wrong-secret").update(input).digest("base64url");
      const res = await app.inject({
        method: "GET",
        url: "/auth/me",
        headers: { authorization: `Bearer ${input}.${badSig}` },
      });
      assert.equal(res.statusCode, 401);
    } finally {
      await app.close();
    }
  });
});
