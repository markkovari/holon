// Host shim for `wasi:keyvalue/store@0.2.0-draft`.
//
// When the component is embedded in-process (jco), the host — this Node code —
// must satisfy every WASI import the guest expects. jco provides the standard
// wasi:cli/clocks/io/random/http/filesystem shims automatically; the
// NON-standard imports (keyvalue store + atomics) are supplied here.
//
// This is a trivial in-memory store. Swap the Map for redis/sqlite/NATS to make
// it real — the component neither knows nor cares.
//
// `buckets` is exported so the atomics shim can mutate the SAME store: a
// borrowed Bucket handed to atomics.increment reads/writes these maps.

export const buckets = new Map(); // name -> Map<string, Uint8Array>

class Bucket {
  constructor(name) {
    if (!buckets.has(name)) buckets.set(name, new Map());
    this.store = buckets.get(name);
  }
  get(key) {
    return this.store.get(key); // Uint8Array | undefined  (maps to option<list<u8>>)
  }
  set(key, value) {
    this.store.set(key, value);
  }
  delete(key) {
    this.store.delete(key);
  }
  exists(key) {
    return this.store.has(key);
  }
  listKeys(_cursor) {
    return { keys: [...this.store.keys()], cursor: undefined };
  }
}

// jco imports these as flat named exports of the mapped module.
export { Bucket };
export function open(name) {
  return new Bucket(name);
}
