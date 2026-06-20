// Durability test: data written through the SQLite backend survives a
// "restart". We can't truly re-exec the process in one test run, but we prove
// the round-trip that makes a restart safe: write via the backend, then a fresh
// load() (what the shim does on boot) returns the same bytes — and the on-disk
// file is what carries it across process lifetimes.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const dbFile = path.join(here, "..", ".persist-test.sqlite");

describe("sqlite kv backend durability", () => {
  before(() => {
    process.env.VET_SQLITE_PATH = dbFile;
    for (const f of [dbFile, `${dbFile}-wal`, `${dbFile}-shm`]) {
      if (fs.existsSync(f)) fs.rmSync(f);
    }
  });
  after(() => {
    for (const f of [dbFile, `${dbFile}-wal`, `${dbFile}-shm`]) {
      if (fs.existsSync(f)) fs.rmSync(f);
    }
  });

  it("write-through persists and a fresh load() reads it back", async () => {
    const enc = new TextEncoder();
    const dec = new TextDecoder();

    // First "process": write a pet record through the backend.
    const b1 = await import(`../kv-backend.js?v=1`);
    await b1.write("default", "pet_rex", enc.encode(JSON.stringify({ name: "Rex" })));
    await b1.write("default", "appt_1", enc.encode(JSON.stringify({ pet: "pet_rex" })));
    await b1.remove("default", "appt_1");

    // Second "process": fresh module instance, same on-disk file -> load().
    const b2 = await import(`../kv-backend.js?v=2`);
    const store = await b2.load();
    const def = store.get("default");
    assert.ok(def, "default bucket present after reload");
    assert.ok(def.has("pet_rex"), "pet survives reload");
    assert.equal(dec.decode(def.get("pet_rex")), JSON.stringify({ name: "Rex" }));
    assert.ok(!def.has("appt_1"), "deleted key stays deleted after reload");
  });
});
