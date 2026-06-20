// NATS JetStream KV durable backend for the vet-clinic KV shim.
//
// `wasi:keyvalue@0.2.0-draft` is SYNCHRONOUS, but NATS is async — so we can't
// call NATS inline from the shim's get/set. The shim keeps a synchronous
// in-memory MIRROR for reads and writes-through to this backend asynchronously
// on each mutation. The uniform backend interface (load/write/remove) is the
// same across all persistence variants:
//   - load()  hydrates the mirror on boot (full store)
//   - write() persists a set
//   - remove() persists a delete
//
// This is the SAME store the wasmCloud `keyvalue-nats` provider uses, so this
// jco-side backend is the closest mirror of the production storage path.
//
// Encoding: everything lives in a single JetStream KV bucket
// (VET_NATS_KV, default "vetclinic"). The app's (bucket, key) pair is encoded
// into the NATS KV ENTRY key as `${bucket}/${key}` — NATS KV keys allow
// `A-Za-z0-9-_/=.`, and the app's keys (pet_, appt_, sess_, al_, ...) plus the
// "default" app-bucket are all within that set. `/` is the separator (allowed,
// and the app bucket never contains one, so the FIRST `/` split is unambiguous).
// If an app key ever contains a disallowed char we base64url-encode just the key
// part and prefix it with "b64." so load() can decode it back (defensive — the
// app's keys are kv-safe and won't trigger this). Values are raw bytes: NATS KV
// stores Uint8Array natively, so NO base64 is used for values.

import { connect } from "nats";

// NOTE: the nats client's `localhost` resolution can hang on some hosts; the
// loopback IP connects instantly, so default to 127.0.0.1 (override freely).
const servers = process.env.VET_NATS_URL ?? "nats://127.0.0.1:4222";
const bucketName = process.env.VET_NATS_KV ?? "vetclinic";

let nc, js, kv;

// Lazy async init shared by load/write/remove. `js.views.kv(name)` creates the
// bucket if absent (requires JetStream enabled on the server). We give connect
// an explicit timeout + a few retries: a slow first connect must NOT silently
// fall through to "no backend" (which would leave the app on mirror-only and
// lose data on restart).
const ready = (async () => {
  let lastErr;
  for (let attempt = 1; attempt <= 5; attempt++) {
    try {
      nc = await connect({ servers: servers.split(","), timeout: 5000, maxReconnectAttempts: -1 });
      js = nc.jetstream();
      kv = await js.views.kv(bucketName, { history: 1 });
      return;
    } catch (e) {
      lastErr = e;
      await new Promise((r) => setTimeout(r, 500 * attempt));
    }
  }
  throw new Error(`NATS KV backend unreachable after retries: ${lastErr?.message ?? lastErr}`);
})();

// NATS KV key charset: A-Za-z0-9-_/=. — guard the key part defensively.
const KV_SAFE = /^[A-Za-z0-9\-_=.]+$/;

function encodeEntryKey(bucket, key) {
  const safeKey = KV_SAFE.test(key)
    ? key
    : "b64." + Buffer.from(key).toString("base64url");
  return `${bucket}/${safeKey}`;
}

function decodeEntryKey(entryKey) {
  const idx = entryKey.indexOf("/");
  const bucket = entryKey.slice(0, idx);
  let key = entryKey.slice(idx + 1);
  if (key.startsWith("b64.")) {
    key = Buffer.from(key.slice(4), "base64url").toString();
  }
  return [bucket, key];
}

export async function load() {
  await ready;
  const out = new Map();

  // Collect ALL keys FIRST, then fetch values. Interleaving kv.get() inside the
  // `for await` over kv.keys() breaks the keys() consumer (it shares the
  // connection's ordered-consumer machinery), truncating the iteration to the
  // first key — so values must be fetched in a second pass.
  const allKeys = [];
  try {
    for await (const k of await kv.keys()) allKeys.push(k);
  } catch {
    return out; // bucket just created / empty
  }

  for (const k of allKeys) {
    const e = await kv.get(k);
    if (!e) continue;
    const [bucket, key] = decodeEntryKey(k);
    if (!out.has(bucket)) out.set(bucket, new Map());
    out.get(bucket).set(key, new Uint8Array(e.value)); // native bytes
  }
  return out;
}

export async function write(bucket, key, value) {
  await ready;
  await kv.put(encodeEntryKey(bucket, key), value);
}

export async function remove(bucket, key) {
  await ready;
  await kv.delete(encodeEntryKey(bucket, key));
}
