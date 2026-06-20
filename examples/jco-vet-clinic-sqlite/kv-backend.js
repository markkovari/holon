// SQLite durable backend for the vet-clinic KV shim.
//
// better-sqlite3 is fully SYNCHRONOUS, which fits `wasi:keyvalue@0.2.0-draft`'s
// synchronous get/set perfectly — but the shim's write-through interface is
// uniform across backends (load/write/remove), so we implement all three. The
// whole store is one table `(bucket, key, value BLOB)`; `load()` hydrates the
// shim's mirror on boot; `write`/`remove` persist each mutation. Data lives in
// vet-clinic.sqlite (override with VET_SQLITE_PATH) and survives restarts.

import Database from "better-sqlite3";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const dbPath = process.env.VET_SQLITE_PATH ?? path.join(here, "vet-clinic.sqlite");
const db = new Database(dbPath);
db.pragma("journal_mode = WAL");
db.exec(
  "CREATE TABLE IF NOT EXISTS kv (bucket TEXT NOT NULL, key TEXT NOT NULL, value BLOB NOT NULL, PRIMARY KEY (bucket, key))",
);

const selAll = db.prepare("SELECT bucket, key, value FROM kv");
const upsert = db.prepare(
  "INSERT INTO kv (bucket, key, value) VALUES (?, ?, ?) ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value",
);
const del = db.prepare("DELETE FROM kv WHERE bucket = ? AND key = ?");

export async function load() {
  const out = new Map();
  for (const row of selAll.all()) {
    if (!out.has(row.bucket)) out.set(row.bucket, new Map());
    // better-sqlite3 returns a Node Buffer for BLOB; the shim wants Uint8Array.
    out.get(row.bucket).set(row.key, new Uint8Array(row.value));
  }
  return out;
}

export async function write(bucket, key, value) {
  upsert.run(bucket, key, Buffer.from(value));
}

export async function remove(bucket, key) {
  del.run(bucket, key);
}
