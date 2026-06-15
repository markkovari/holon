// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies feature-flag definitions the OAM `config:` block would on wasmCloud.
// Flag keys are `flag:{name}` -> boolean (`true`/`false`/`on`/`off`/`1`/`0`)
// or a percentage (`N%`, bucketed by a stable hash of the subject).

const values = {
  "flag:new-checkout": process.env.FLAG_NEW_CHECKOUT ?? "true",
  "flag:dark-mode": process.env.FLAG_DARK_MODE ?? "false",
  "flag:beta-search": process.env.FLAG_BETA_SEARCH ?? "25%",
};

// jco imports these as flat named exports. `get` returns option<string>.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
