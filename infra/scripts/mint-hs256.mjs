#!/usr/bin/env node
// Mint an HS256 JWT for testing the auth-guard JWT path locally — no IdP needed.
//
// The token is signed with the same secret you seed into the keyvalue store as
// `hs256-secret`, and auth-guard must allow HS256 (config `allowed-algs` must
// include HS256 — it does NOT by default; set it for dev/test).
//
// Usage:
//   node mint-hs256.mjs --secret dev-secret --sub user-1 --tenant acme \
//        --iss https://local --aud comp-auth --scope "orders:read profile"
//
// Prints the token. Present it as `Authorization: Bearer <token>`.

import crypto from "node:crypto";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, arr) => {
    if (a.startsWith("--")) acc.push([a.slice(2), arr[i + 1]]);
    return acc;
  }, []),
);

const secret = args.secret ?? "dev-only-shared-secret-change-me";
const now = Math.floor(Date.now() / 1000);
const ttl = Number(args.ttl ?? 3600);

const b64 = (obj) =>
  Buffer.from(JSON.stringify(obj)).toString("base64url");

const header = { alg: "HS256", typ: "JWT" };
const payload = {
  sub: args.sub ?? "dev-user",
  iss: args.iss ?? "https://local",
  aud: args.aud ?? "comp-auth",
  iat: now,
  nbf: now,
  exp: now + ttl,
  ...(args.tenant ? { tenant: args.tenant } : {}),
  ...(args.scope ? { scope: args.scope } : {}),
};

const signingInput = `${b64(header)}.${b64(payload)}`;
const sig = crypto
  .createHmac("sha256", secret)
  .update(signingInput)
  .digest("base64url");

console.log(`${signingInput}.${sig}`);
