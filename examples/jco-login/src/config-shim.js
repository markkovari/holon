// Host shim for `wasi:config/runtime@0.2.0-draft`.
//
// In this composition only the secrets:vault sub-component reads wasi:config —
// it fetches `master-key`, the 32-byte AEAD master key (base64 STANDARD) used
// for envelope encryption / pepper derivation. The config:store sub-component
// reads session-ttl from its OWN keyvalue store, not from here, and login-app
// reads nothing from wasi:config. So `master-key` is the only key needed.
//
// The value below is a THROWAWAY TEST KEY ONLY (base64 of 32 zero bytes). Real
// deployments inject a real secret via wasi:config — never ship the fallback.

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
