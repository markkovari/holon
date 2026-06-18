// Host shim for `wasi:config/runtime@0.2.0-draft`.
// Supplies the same knobs the OAM `config:` block would on wasmCloud.
//
// `master-key` is the 32-byte AEAD master key (base64 STANDARD) used by the
// vault for envelope encryption. The value below is a THROWAWAY TEST KEY ONLY
// (32 zero bytes). Real deployments inject a real secret via wasi:config —
// never ship the hardcoded fallback.

const values = {
  "master-key":
    process.env.MASTER_KEY ?? "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
};

// jco imports these as flat named exports. `get` returns option<string>.
export function get(key) {
  const v = values[key];
  return v === undefined ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
