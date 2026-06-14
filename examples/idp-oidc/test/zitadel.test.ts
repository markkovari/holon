// Zitadel OIDC reachability + verification path.
//
// Minting a Zitadel JWT non-interactively needs a machine user + JWT-profile
// key (an admin-authenticated, multi-step setup) — documented in the README as
// the production step. What we CAN prove deterministically here: auth-guard's
// verifier is issuer-agnostic (it follows the token's `iss` to discovery +
// JWKS), and Zitadel exposes a standard OIDC discovery doc + JWKS the verifier
// consumes — identical to the Hydra path that's fully exercised in hydra.test.
//
// If a real Zitadel JWT is provided via ZITADEL_TOKEN, we verify it for real.

import { describe, it } from "node:test";
import assert from "node:assert/strict";

const ISSUER = process.env.ZITADEL_ISSUER ?? "http://localhost:8080";

async function zitadelUp(): Promise<boolean> {
  try {
    const r = await fetch(`${ISSUER}/.well-known/openid-configuration`, {
      signal: AbortSignal.timeout(2000),
    });
    return r.ok;
  } catch {
    return false;
  }
}

const up = await zitadelUp();

describe("Zitadel OIDC", { skip: up ? false : `Zitadel unreachable at ${ISSUER}` }, () => {
  it("exposes a standard OIDC discovery document", async () => {
    const cfg = (await (await fetch(`${ISSUER}/.well-known/openid-configuration`)).json()) as {
      issuer: string;
      jwks_uri: string;
      token_endpoint: string;
    };
    assert.equal(cfg.issuer, ISSUER);
    assert.ok(cfg.jwks_uri, "advertises a jwks_uri");
    assert.ok(cfg.token_endpoint, "advertises a token_endpoint");
  });

  it("exposes a JWKS endpoint the verifier resolves keys from", async () => {
    const cfg = (await (await fetch(`${ISSUER}/.well-known/openid-configuration`)).json()) as {
      jwks_uri: string;
    };
    const jwks = (await (await fetch(cfg.jwks_uri)).json()) as { keys: unknown[] };
    assert.ok(Array.isArray(jwks.keys), "JWKS has a keys array");
    // keys may be lazily populated until the first signing key is used; the
    // shape being correct is what the verifier needs.
  });

  it("verifies a real Zitadel JWT if ZITADEL_TOKEN is supplied", async () => {
    const token = process.env.ZITADEL_TOKEN;
    if (!token) {
      // documented manual step — skip when no token provided
      return;
    }
    process.env.EXPECTED_ISSUER = ISSUER;
    process.env.ALLOWED_ALGS = "RS256,ES256";
    const { authorizer } = await import("../gen/auth_guard.js");
    const p = authorizer.introspect(token) as { subject: string };
    assert.ok(p.subject.length > 0);
  });
});
