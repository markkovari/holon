// Durability test: data written through the NATS JetStream KV backend survives
// a "restart". We can't truly re-exec the process in one test run, but we prove
// the round-trip that makes a restart safe: write via the backend, then a fresh
// load() (what the shim does on boot) returns the same bytes — and the NATS KV
// bucket is what carries it across process lifetimes.
//
// Uses a UNIQUE test bucket (VET_NATS_KV) so it never clobbers a real run, and
// destroys it in after(). Skips gracefully if NATS/JetStream is unreachable.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { connect } from "nats";

const TEST_BUCKET = "vetclinic_test_persist";
const url = process.env.VET_NATS_URL ?? "nats://127.0.0.1:4222";

// Probe NATS + JetStream once; if unavailable, skip the suite with a clear msg.
let available = false;
let skipReason = "";

before(async () => {
  process.env.VET_NATS_KV = TEST_BUCKET;
  process.env.VET_NATS_URL = url;
  let nc;
  try {
    nc = await connect({ servers: url.split(","), timeout: 2000 });
  } catch (err) {
    skipReason = `NATS unreachable at ${url} (${err?.message ?? err}) — start one with: docker run -d --name vet-nats -p 4222:4222 nats:2.10-alpine -js`;
    return;
  }
  try {
    const js = nc.jetstream();
    // Creating the KV bucket fails if JetStream isn't enabled on the server.
    const kv = await js.views.kv(TEST_BUCKET, { history: 1 });
    await kv.destroy().catch(() => {}); // start from a clean bucket
    available = true;
  } catch (err) {
    skipReason = `JetStream not enabled at ${url} (${err?.message ?? err}) — start one with: docker run -d --name vet-nats -p 4222:4222 nats:2.10-alpine -js`;
  } finally {
    await nc.close();
  }
});

after(async () => {
  if (!available) return;
  try {
    const nc = await connect({ servers: url.split(","), timeout: 2000 });
    const js = nc.jetstream();
    const kv = await js.views.kv(TEST_BUCKET, { history: 1 });
    await kv.destroy().catch(() => {});
    await nc.close();
  } catch {
    // best-effort cleanup
  }
});

describe("nats jetstream kv backend durability", () => {
  it("write-through persists and a fresh load() reads it back", async (t) => {
    if (!available) {
      t.skip(skipReason);
      return;
    }
    const enc = new TextEncoder();
    const dec = new TextDecoder();

    // First "process": write a pet record through the backend, then delete an appt.
    const b1 = await import(`../kv-backend.js?v=1`);
    await b1.write("default", "pet_rex", enc.encode(JSON.stringify({ name: "Rex" })));
    await b1.write("default", "appt_1", enc.encode(JSON.stringify({ pet: "pet_rex" })));
    await b1.remove("default", "appt_1");

    // Second "process": fresh module instance, same NATS KV bucket -> load().
    const b2 = await import(`../kv-backend.js?v=2`);
    const store = await b2.load();
    const def = store.get("default");
    assert.ok(def, "default bucket present after reload");
    assert.ok(def.has("pet_rex"), "pet survives reload");
    assert.equal(dec.decode(def.get("pet_rex")), JSON.stringify({ name: "Rex" }));
    assert.ok(def.get("pet_rex") instanceof Uint8Array, "value is native bytes (Uint8Array)");
    assert.ok(!def.has("appt_1"), "deleted key stays deleted after reload");
  });
});
