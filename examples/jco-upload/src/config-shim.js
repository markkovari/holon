// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies the same knobs the OAM `config:` block would on wasmCloud.

const values = {
  "allowed-types": process.env.ALLOWED_TYPES ?? "image/png,image/jpeg,application/pdf",
  "max-size": process.env.MAX_SIZE ?? "10485760",
  "ticket-ttl": process.env.TICKET_TTL ?? "300",
  "ticket-secret": process.env.TICKET_SECRET ?? "test-upload-secret",
};

// jco imports these as flat named exports. `get` returns option<string>.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
