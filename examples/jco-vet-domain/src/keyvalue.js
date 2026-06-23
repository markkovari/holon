// Host shim for `wasi:keyvalue/store@0.2.0-draft` + `/atomics`.
//
// Every transpiled component (composed auth-guard, session-store, search-index,
// validate, config-store, notify-dispatch) maps its keyvalue import to THIS
// module, so they all read/write one store. That shared store is the trick that
// lets the components cooperate: a pet written under `pet_*` is indexed by
// search-index and sits beside the sessions auth-guard mints + the audit events
// it records — one store, no network between components.
//
// IMPORTANT: `wasi:keyvalue@0.2.0-draft` get/set/exists/listKeys are SYNCHRONOUS
// (the guest calls them and expects a value back immediately). So an async store
// (Redis, NATS) cannot be called inline. The pattern here: an in-memory MIRROR
// (Map) is the synchronous read/write path; a pluggable DURABLE backend is
// hydrated into the mirror on boot and written through asynchronously on every
// mutation. The variant examples (jco-vet-clinic-{sqlite,redis,nats}) set
// VET_KV_BACKEND to pick the backend; the default is in-memory (non-durable).
//
// A backend module exports:
//   load()        -> async, returns Map<bucketName, Map<key(string), Uint8Array>>
//   write(b,k,v)  -> persist a set (k: string, v: Uint8Array)
//   remove(b,k)   -> persist a delete
// All variants reuse this same shim; only the backend differs.

const buckets = new Map(); // name -> Map<string(key), Uint8Array>
let backend = null; // { write(bucket,key,val), remove(bucket,key) } or null
const inflight = new Set(); // pending async write-through promises
if (process.env.VET_KV_DEBUG) console.error(`[kv-shim] module instance loaded @ ${import.meta.url}`);

// Track a write-through promise so drainBackend() can await it. WASI kv is
// synchronous, so writes to an async backend (Redis/NATS) are fire-and-forget;
// without draining, an abrupt process exit loses in-flight writes. (SQLite is
// synchronous and never has in-flight writes.)
function track(p) {
  const wrapped = Promise.resolve(p).catch(logErr).finally(() => inflight.delete(wrapped));
  inflight.add(wrapped);
}

/** Await all pending write-through operations. Call before a graceful exit. */
export async function drainBackend() {
  while (inflight.size) await Promise.all([...inflight]);
}

function bucketMap(name) {
  if (!buckets.has(name)) buckets.set(name, new Map());
  return buckets.get(name);
}

/**
 * Install a durable backend + hydrate the mirror from it. Call once before the
 * server starts handling requests. Backend resolved from VET_KV_BACKEND
 * (memory|sqlite|redis|nats) unless one is passed explicitly. The backend
 * modules live in the VARIANT example dirs and are loaded lazily so the base
 * app has no hard dependency on redis/sqlite/nats packages.
 */
export async function initBackend(loader) {
  const kind = process.env.VET_KV_BACKEND ?? "memory";
  if (kind === "memory" && !loader) return; // nothing to do
  const mod = loader ? await loader() : null;
  if (!mod) return;
  const initial = await mod.load();
  if (initial) {
    for (const [b, kv] of initial) {
      const m = bucketMap(b);
      for (const [k, v] of kv) m.set(k, v);
    }
  }
  backend = mod;
  if (process.env.VET_KV_DEBUG)
    console.error(`[kv-shim] backend installed (${kind}); write=${typeof mod.write} hydrated buckets=${[...buckets.keys()].join(",")}`);
}

class Bucket {
  constructor(name) {
    this.name = name;
    this.store = bucketMap(name);
  }
  get(key) {
    return this.store.get(key); // Uint8Array | undefined (option<list<u8>>)
  }
  set(key, value) {
    this.store.set(key, value);
    if (backend) track(backend.write(this.name, key, value));
  }
  delete(key) {
    this.store.delete(key);
    if (backend) track(backend.remove(this.name, key));
  }
  exists(key) {
    return this.store.has(key);
  }
  listKeys(_cursor) {
    return { keys: [...this.store.keys()], cursor: undefined };
  }
}

function logErr(e) {
  // Durability is best-effort write-through; a backend hiccup must not break
  // the synchronous guest call. Log + continue (the mirror is still correct).
  console.error("[kv-backend] write-through failed:", e?.message ?? e);
}

export { Bucket };
export function open(name) {
  return new Bucket(name);
}

// wasi:keyvalue/atomics — rate-limiter + quota counters. Single Node process =>
// naturally atomic over the mirror; the new value is written through too.
export function increment(bucket, key, delta) {
  const cur = bucket.get(key);
  const n = (cur ? Number(new TextDecoder().decode(cur)) : 0) + Number(delta);
  bucket.set(key, new TextEncoder().encode(String(n)));
  return BigInt(n);
}

// Test/dev helper: wipe the in-memory mirror (does not touch a durable backend).
export function __reset() {
  buckets.clear();
}
