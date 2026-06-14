// Verify a REAL IdP-issued JWT in-process, against the IdP's LIVE JWKS.
//
// The embedded auth-guard (jco) does the work: introspect(token) detects a JWS,
// reads its `iss`, fetches {iss}/.well-known/openid-configuration + jwks_uri
// over wasi:http (jco's preview2-shim backs this with real fetch), verifies the
// RS256 signature, checks exp/nbf/iss, and returns the principal.
//
// Token source: TOKEN env, else mint one from Ory Hydra via client_credentials
// (HYDRA_ADMIN/HYDRA_PUBLIC, defaults to localhost:4445/4444).

import { authorizer } from "../gen/auth_guard.js";

async function hydraToken(): Promise<{ token: string; issuer: string }> {
  const admin = process.env.HYDRA_ADMIN ?? "http://localhost:4445";
  const pub = process.env.HYDRA_PUBLIC ?? "http://localhost:4444";
  // register an ephemeral client + mint a JWT access token
  const reg = await fetch(`${admin}/admin/clients`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      grant_types: ["client_credentials"],
      token_endpoint_auth_method: "client_secret_post",
      scope: "openid offline",
    }),
  });
  const client = (await reg.json()) as { client_id: string; client_secret: string };
  const tokRes = await fetch(`${pub}/oauth2/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "client_credentials",
      client_id: client.client_id,
      client_secret: client.client_secret,
      scope: "openid",
    }),
  });
  const tok = (await tokRes.json()) as { access_token: string };
  return { token: tok.access_token, issuer: pub };
}

async function main() {
  const token = process.env.TOKEN ?? (await hydraToken()).token;

  // sanity: it must be a JWS (3 segments)
  if (token.split(".").length !== 3) {
    console.error("token is not a JWT (opaque?). Hydra needs STRATEGIES_ACCESS_TOKEN=jwt.");
    process.exit(1);
  }
  const claims = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString());
  console.log(`token iss=${claims.iss} sub=${claims.sub} alg(header)=RS256`);

  // The auth-guard verifies it against the issuer's live JWKS.
  const principal = authorizer.introspect(token);
  console.log(
    "VERIFIED principal:",
    JSON.stringify(principal, (_k, v) => (typeof v === "bigint" ? Number(v) : v)),
  );
}

main().catch((e) => {
  console.error("verify failed:", e);
  process.exit(1);
});
