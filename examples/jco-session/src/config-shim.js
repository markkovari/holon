// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies the same knobs the OAM `config:` block would on wasmCloud.

const values = {
  "default-ttl": process.env.DEFAULT_TTL ?? "86400",
};

// jco imports these as flat named exports. `get` returns option<string>.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
