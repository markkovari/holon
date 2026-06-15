// Host shim for `wasi:config/runtime@0.2.0-draft`. The composed-in
// idempotency-guard reads `default-ttl`; nothing else is needed here.

const values = {
  "default-ttl": process.env.DEFAULT_TTL ?? "86400",
};

export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
