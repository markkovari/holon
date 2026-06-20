// Redis durable backend for the vet-clinic KV shim.
//
// `wasi:keyvalue@0.2.0-draft` is SYNCHRONOUS (get/set return immediately), but
// Redis is async, so we can't call it inline. The shared shim
// (../jco-vet-clinic/src/shims/keyvalue.js) keeps a synchronous in-memory
// MIRROR for reads and WRITES THROUGH to a pluggable durable backend: it
// hydrates the mirror from load() on boot and fires write/remove on each
// mutation. node-redis pipelines/queues commands internally, so the shim's
// fire-and-forget write-through is fine — commands are sent in order.
//
// Storage scheme: each value lives at Redis key `vet:{bucket}:{key}`. Values are
// arbitrary bytes (Uint8Array), so we base64-encode on write and decode on load
// to round-trip exactly through node-redis's default string handling. Data lives
// in the Redis at VET_REDIS_URL (default redis://localhost:6379) and survives
// restarts.

import { createClient } from "redis";

const url = process.env.VET_REDIS_URL ?? "redis://localhost:6379";
const client = createClient({ url });
client.on("error", (err) => console.error("redis client error:", err));

// node-redis v4 must connect before use. load() is awaited first, but write/
// remove may race it, so every entry point awaits this single lazy connect.
const ready = (async () => {
  await client.connect();
})();

export async function load() {
  await ready;
  const out = new Map();
  // SCAN avoids blocking the server on a large keyspace.
  for await (const rawKey of client.scanIterator({ MATCH: "vet:*", COUNT: 500 })) {
    // scanIterator may yield a single key or an array depending on version.
    const keys = Array.isArray(rawKey) ? rawKey : [rawKey];
    for (const fullKey of keys) {
      const b64 = await client.get(fullKey);
      if (b64 == null) continue;
      // Parse `vet:{bucket}:{key}`. The key part may itself contain ':', so we
      // strip the `vet:` prefix then split the remainder on the FIRST ':' only.
      const rest = fullKey.slice("vet:".length);
      const sep = rest.indexOf(":");
      if (sep < 0) continue;
      const bucket = rest.slice(0, sep);
      const key = rest.slice(sep + 1);
      if (!out.has(bucket)) out.set(bucket, new Map());
      out.get(bucket).set(key, new Uint8Array(Buffer.from(b64, "base64")));
    }
  }
  return out;
}

export async function write(bucket, key, value) {
  await ready;
  await client.set(`vet:${bucket}:${key}`, Buffer.from(value).toString("base64"));
}

export async function remove(bucket, key) {
  await ready;
  await client.del(`vet:${bucket}:${key}`);
}
