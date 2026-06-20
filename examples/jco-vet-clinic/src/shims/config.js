// Shared host shim for `wasi:config/runtime@0.2.0-draft`.
//
// Supplies the deployment knobs every composed/transpiled component reads. In
// production these come from the wasmCloud `config:` block or env; here they're
// literals (overridable via env) so the example runs with zero setup.

const values = {
  // --- auth-guard policy ---
  "default-tenant": process.env.VET_TENANT ?? "acme-vet",
  "session-ttl": process.env.SESSION_TTL ?? "3600",
  "password-min-len": "8",
  "audit-enabled": "true",
  "max-attempts": "5",
  "lockout-window": "300",

  // --- notify-dispatch gateways ---
  // No real vendor: point email/sms at a local sink so `send` is a no-op-ish
  // log rather than a live POST. The example treats a reachable-or-not result
  // as "queued"; it never depends on a real upstream.
  "notify:email-url": process.env.NOTIFY_EMAIL_URL ?? "http://localhost:9/sink",
  "notify:sms-url": process.env.NOTIFY_SMS_URL ?? "http://localhost:9/sink",

  // --- upload-policy (pet photos) ---
  "allowed-types": process.env.UPLOAD_TYPES ?? "image/png,image/jpeg,image/webp,image/gif",
  "max-size": process.env.UPLOAD_MAX ?? "2097152", // 2 MiB
  "ticket-ttl": "300",
  "ticket-secret": process.env.UPLOAD_SECRET ?? "vet-upload-secret",
};

export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
