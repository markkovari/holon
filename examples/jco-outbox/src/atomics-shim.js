// Host shim for `wasi:keyvalue/atomics@0.2.0-draft`.
//
// Companion to keyvalue-shim.js: atomics operates on the SAME in-memory store.
// `increment` receives a borrowed Bucket instance (from keyvalue-shim) plus a
// key and a delta, atomically bumps the integer stored at that key, and returns
// the new value. Because Node is single-threaded here the read-modify-write is
// trivially atomic; a real backend (redis INCRBY, SQL UPDATE ... RETURNING)
// would supply the same contract.
//
// If outbox.wasm's transpiled code never actually imports atomics, this map is
// harmless dead weight — but it must exist so the --map resolves.

const dec = new TextDecoder();
const enc = new TextEncoder();

// increment(bucket: borrow<bucket>, key: string, delta: u64) -> u64
export function increment(bucket, key, delta) {
  const cur = bucket.get(key); // Uint8Array | undefined
  const base = cur === undefined ? 0n : BigInt(parseInt(dec.decode(cur), 10) || 0);
  const next = base + BigInt(delta);
  bucket.set(key, enc.encode(next.toString()));
  return next; // bigint -> u64
}
