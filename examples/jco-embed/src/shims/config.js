// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies the same policy knobs the OAM `config:` block would on wasmCloud.
// Override via env to demonstrate the component reading config at runtime.

const values = {
  "session-ttl": process.env.SESSION_TTL ?? "3600",
  "password-min-len": process.env.PASSWORD_MIN_LEN ?? "8",
  "jwks-cache-ttl": process.env.JWKS_CACHE_TTL ?? "3600",
  "default-tenant": process.env.DEFAULT_TENANT ?? "",
  // consumed by the composed rate-limiter component
  "max-attempts": process.env.MAX_ATTEMPTS ?? "5",
  "lockout-window": process.env.LOCKOUT_WINDOW ?? "300",
};

// jco imports these as flat named exports. `get` returns option<string>
// (a value or undefined); throwing would map to the config-error variant.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
