// Durability test: data written through the Redis backend survives a "restart".
// We can't truly re-exec the process in one test run, but we prove the round-trip
// that makes a restart safe: write via the backend, then a FRESH load() (what the
// shim does on boot, here a re-imported module instance) returns the same bytes —
// and Redis is what carries it across process lifetimes.
//
// Skips gracefully if no Redis is reachable on VET_REDIS_URL.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { createClient } from "redis";

const url = process.env.VET_REDIS_URL ?? "redis://localhost:6379";

// Probe Redis up-front so we can skip the whole suite if it's down.
let redisUp = false;
{
  const probe = createClient({ url });
  probe.on("error", () => {});
  try {
    await probe.connect();
    await probe.ping();
    redisUp = true;
    await probe.quit();
  } catch {
    redisUp = false;
    try {
      await probe.disconnect();
    } catch {}
  }
}

describe("redis kv backend durability", { skip: redisUp ? false : "no Redis reachable on " + url }, () => {
  const cleaner = createClient({ url });

  before(async () => {
    cleaner.on("error", () => {});
    await cleaner.connect();
    // Start clean so a stale run can't mask a bug.
    await cleaner.del("vet:default:pet_rex");
    await cleaner.del("vet:default:appt_1");
  });

  after(async () => {
    await cleaner.del("vet:default:pet_rex");
    await cleaner.del("vet:default:appt_1");
    await cleaner.quit();
  });

  it("write-through persists and a fresh load() reads it back", async () => {
    const enc = new TextEncoder();
    const dec = new TextDecoder();

    // First "process": write a pet record through the backend, then delete an appt.
    const b1 = await import(`../kv-backend.js?v=1`);
    await b1.write("default", "pet_rex", enc.encode(JSON.stringify({ name: "Rex" })));
    await b1.write("default", "appt_1", enc.encode(JSON.stringify({ pet: "pet_rex" })));
    await b1.remove("default", "appt_1");

    // Second "process": fresh module instance, same Redis -> load().
    const b2 = await import(`../kv-backend.js?v=2`);
    const store = await b2.load();
    const def = store.get("default");
    assert.ok(def, "default bucket present after reload");
    assert.ok(def.has("pet_rex"), "pet survives reload");
    assert.equal(dec.decode(def.get("pet_rex")), JSON.stringify({ name: "Rex" }));
    assert.ok(!def.has("appt_1"), "deleted key stays deleted after reload");
  });
});
