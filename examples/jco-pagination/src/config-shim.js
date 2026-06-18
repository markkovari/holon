// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies the same knobs the OAM `config:` block would on wasmCloud.

const values = {
  "cursor-secret": process.env.CURSOR_SECRET ?? "test-cursor-secret",
  "max-page-size": process.env.MAX_PAGE_SIZE ?? "100",
};

// jco imports these as flat named exports. `get` returns option<string>.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
