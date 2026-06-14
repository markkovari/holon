// wasi:config/runtime shim for the IdP example. The key bits for verifying a
// real IdP token: expected-issuer (must equal the token's `iss`), allowed-algs
// (RS256 — the IdP signs with RSA), and audit off for clean output.
const values = {
  "expected-issuer": process.env.EXPECTED_ISSUER ?? "",
  "expected-audience": process.env.EXPECTED_AUDIENCE ?? "",
  "allowed-algs": process.env.ALLOWED_ALGS ?? "RS256,ES256",
  "jwks-cache-ttl": process.env.JWKS_CACHE_TTL ?? "3600",
  "clock-skew": process.env.CLOCK_SKEW ?? "60",
  "default-tenant": process.env.DEFAULT_TENANT ?? "",
  "audit-enabled": process.env.AUDIT_ENABLED ?? "false",
};
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
