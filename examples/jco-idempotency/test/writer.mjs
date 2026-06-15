// Child-process writer for the durability test. Runs in its OWN process against
// the SQLite-backed shim, stores a completed idempotency record, then exits.
// The parent test then starts fresh (separate process) and must see the record
// — proving the wasm SQLite store survived a "restart", which the Map shim
// cannot do.
//
// Usage: node test/writer.mjs <KV_SQLITE_PATH> <key> <status> <body>

const [, , dbPath, key, status, body] = process.argv;
process.env.KV_SQLITE_PATH = dbPath;

const { store: idem } = await import("../gen-sqlite/idempotency_guard.js");

const enc = (s) => new TextEncoder().encode(s);

idem.begin(key, 3600n); // reserve
idem.complete(key, Number(status), enc(body)); // persist to SQLite file
