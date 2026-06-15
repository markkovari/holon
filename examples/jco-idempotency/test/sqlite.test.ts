// Durability test for the SQLite-wasm (sql.js) keyvalue shim.
//
// The SAME idempotency_guard.wasm component, transpiled with the SQLite shim
// (gen-sqlite/) instead of the in-memory Map (gen/). A child process writes a
// completed record to a temp .sqlite file; this (separate) process then opens
// the component fresh and must REPLAY that record — proving the wasm store
// persisted across a process restart, which the Map shim cannot.

import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

const dbPath = fileURLToPath(new URL("../durability-test.sqlite", import.meta.url));
const writer = fileURLToPath(new URL("./writer.mjs", import.meta.url));

after(() => rmSync(dbPath, { force: true }));

describe("idempotency:guard backed by SQLite-wasm (sql.js)", () => {
  it("replays a record written by a previous process (durable)", async () => {
    // Phase 1: a SEPARATE process writes + completes a key, then exits.
    // writer.mjs imports only generated .js, so plain node runs it (no tsx).
    execFileSync(process.execPath, [
      writer,
      dbPath,
      "payment-xyz",
      "201",
      '{"charged":true}',
    ]);

    // Phase 2: THIS process opens the component fresh against the same file.
    process.env.KV_SQLITE_PATH = dbPath;
    const { store: idem } = await import("../gen-sqlite/idempotency_guard.js");

    const replay = idem.begin("payment-xyz", 3600n);
    assert.ok(replay, "record from the prior process survived");
    assert.equal(replay.status, 201);
    assert.equal(new TextDecoder().decode(replay.body), '{"charged":true}');

    // A never-seen key is still first-caller (none).
    assert.equal(idem.begin("fresh-key", 3600n), undefined);
  });
});
