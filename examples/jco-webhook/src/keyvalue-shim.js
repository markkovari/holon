// Host shim for `wasi:keyvalue/store@0.2.0-draft` (in-memory Map), plus a test
// hook to pre-seed the signing secret the webhook component reads.

const buckets = new Map(); // name -> Map<string, Uint8Array>

function bucketMap(name) {
  if (!buckets.has(name)) buckets.set(name, new Map());
  return buckets.get(name);
}

// test hook (not part of the WIT): seed a raw value into the default bucket.
export function __seed(key, value) {
  bucketMap("default").set(key, new TextEncoder().encode(value));
}

class Bucket {
  constructor(name) {
    this.store = bucketMap(name);
  }
  get(key) {
    return this.store.get(key);
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

export { Bucket };
export function open(name) {
  return new Bucket(name);
}
