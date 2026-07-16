// Host shim for `wasi:config/runtime@0.2.0-draft` — the deployment knobs the
// composed auth-guard reads. In production these come from the wasmCloud
// `config:` block; here they're literals so the example runs with zero setup.

const values = {
  "default-tenant": process.env.HELPDESK_TENANT ?? "helpdesk",
  "session-ttl": process.env.SESSION_TTL ?? "3600",
  "password-min-len": "8",
  "audit-enabled": "true",
  "max-attempts": "5",
  "lockout-window": "300",
};

export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
