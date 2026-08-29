// Host shim for `comp:store/cas@0.1.0` — the compare-and-set `wasi:keyvalue` lacks.
//
// ADR-0065 is why the interface exists: `record-store` used to enforce its revision
// guard with read / compare / write over three `wasi:keyvalue` calls, and anything
// that changed the value in between made the comparison agree with itself about
// state that no longer existed. Three appends went in and two survived. The
// comparison has to happen where the data is, so the store grew one call that does
// both.
//
// In-process under jco, "where the data is" is the `Bucket` from `keyvalue-shim.js`,
// which arrives here as an argument — so this file needs no shared module state and
// stays correct however that shim stores things.
//
// Revisions are per (bucket, key) and start at 1. The WIT says they are opaque and
// compared only for equality, and that nothing may assume they increment by one, so
// a counter is a legal implementation rather than a convenient one.

/// Revisions live per (bucket NAME, key), not per bucket object.
///
/// `store.open()` hands back a fresh `Bucket` on every call, so a `WeakMap` keyed
/// by the object gives each handle its own empty history — and the guard then
/// compares a revision from one handle against a counter in another. Keyed by name,
/// two handles on one bucket see one history, which is what the real store does.
const revisions = new Map(); // bucketName -> Map<key, bigint>

function revs(bucket) {
  // `Bucket` from keyvalue-shim.js holds the backing Map itself; use it as the
  // identity, since two handles on one name share that exact object.
  let m = revisions.get(bucket.store);
  if (m === undefined) {
    m = new Map();
    revisions.set(bucket.store, m);
  }
  return m;
}

/// `result<option<versioned>, error>` — jco hands back the value directly and
/// throws for the error arm, so an absent key is `undefined`.
export function get(bucket, key) {
  const value = bucket.get(key);
  if (value === undefined) return undefined;
  return { revision: revs(bucket).get(key) ?? 1n, value };
}

/// `expected` of 0 means "must not exist yet", so a create and an update are the
/// same call. A conflict reports the revision the key has actually moved to — 0
/// when it is absent — which is what the caller needs to re-read and retry.
export function set(bucket, key, value, expected) {
  // `u64` crosses the jco boundary as a BigInt. Comparing one to a Number is
  // never equal, so a Number counter here made every guarded write report a
  // conflict — the component retried 40 times and gave up with
  // "all attempts lost the race" against a store nothing else was touching.
  const current = bucket.exists(key) ? (revs(bucket).get(key) ?? 1n) : 0n;
  if (current !== BigInt(expected)) return { tag: "conflict", val: current };
  const next = current + 1n;
  bucket.set(key, value);
  revs(bucket).set(key, next);
  return { tag: "committed", val: next };
}
