// Fake backing store shim — satisfies the cache component's `source` (read) and
// `sink` (write) imports for the strategy tests. A plain in-memory Map stands in
// for whatever real datastore a production deployment would back with.
//
// jco maps `cache:store/source` and `cache:store/sink` to this module, so it
// exports the union of both interfaces' flat functions: load / store / remove.

const backing = new Map(); // key -> Uint8Array

// test hooks (not part of the WIT) to drive/inspect the fake store
export const __backing = backing;
export function __seed(key, value) {
  backing.set(key, new TextEncoder().encode(value));
}

// cache:store/source
export function load(key) {
  return backing.get(key); // Uint8Array | undefined  -> option<list<u8>>
}

// cache:store/sink
export function store(key, value) {
  backing.set(key, value);
}
export function remove(key) {
  backing.delete(key);
}
