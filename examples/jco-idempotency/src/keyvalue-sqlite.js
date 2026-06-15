// Host shim for `wasi:keyvalue/store@0.2.0-draft`, backed by SQLite compiled to
// WebAssembly (sql.js). The component imports the SAME `wasi:keyvalue/store`
// interface as with the in-memory Map shim — only the host implementation
// differs. This is the WIT-first thesis made concrete: a different storage
// engine, the identical `.wasm` guest, swapped purely by the transpile `--map`.
//
// Why this is interesting: the *store itself* is a wasm artifact (SQLite built
// to wasm), embedded in the Node host, which in turn embeds the guest component
// (also wasm). Two independent wasm modules meeting at the keyvalue boundary.
//
// Durability: each bucket is a table `(k TEXT PRIMARY KEY, v BLOB)`. Mutations
// persist the whole DB image to `KV_SQLITE_PATH` (default ./idem.sqlite) so
// state survives a process restart — unlike the Map shim. On startup the file
// is loaded back if present.
//
// NB: sql.js init is async, but jco calls these shim functions synchronously.
// We resolve the engine with top-level await (Node ESM) BEFORE the module
// finishes loading, so `open()`/`get()`/... are synchronous by the time the
// component runs.

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import initSqlJs from "sql.js";

const DB_PATH = process.env.KV_SQLITE_PATH ?? "idem.sqlite";

// Resolve the wasm SQLite engine up-front (top-level await).
const SQL = await initSqlJs();

// Load an existing DB image, or start fresh.
const db = existsSync(DB_PATH)
  ? new SQL.Database(readFileSync(DB_PATH))
  : new SQL.Database();

// One physical table per bucket name (sanitized to a safe SQL identifier).
const tableFor = (name) => `kv_${name.replace(/[^A-Za-z0-9_]/g, "_")}`;

function persist() {
  writeFileSync(DB_PATH, Buffer.from(db.export()));
}

class Bucket {
  constructor(name) {
    this.table = tableFor(name);
    db.run(`CREATE TABLE IF NOT EXISTS ${this.table} (k TEXT PRIMARY KEY, v BLOB)`);
  }

  get(key) {
    const stmt = db.prepare(`SELECT v FROM ${this.table} WHERE k = :k`);
    stmt.bind({ ":k": key });
    let out; // undefined -> option<list<u8>> none
    if (stmt.step()) {
      const v = stmt.get()[0]; // Uint8Array (BLOB)
      out = v instanceof Uint8Array ? v : new Uint8Array(v);
    }
    stmt.free();
    return out;
  }

  set(key, value) {
    db.run(`INSERT INTO ${this.table} (k, v) VALUES (:k, :v)
            ON CONFLICT(k) DO UPDATE SET v = :v`, {
      ":k": key,
      ":v": value, // Uint8Array -> BLOB
    });
    persist();
  }

  delete(key) {
    db.run(`DELETE FROM ${this.table} WHERE k = :k`, { ":k": key });
    persist();
  }

  exists(key) {
    const stmt = db.prepare(`SELECT 1 FROM ${this.table} WHERE k = :k`);
    stmt.bind({ ":k": key });
    const found = stmt.step();
    stmt.free();
    return found;
  }

  listKeys(_cursor) {
    const stmt = db.prepare(`SELECT k FROM ${this.table}`);
    const keys = [];
    while (stmt.step()) keys.push(stmt.get()[0]);
    stmt.free();
    return { keys, cursor: undefined };
  }
}

// jco imports these as flat named exports of the mapped module.
export { Bucket };
export function open(name) {
  return new Bucket(name);
}
