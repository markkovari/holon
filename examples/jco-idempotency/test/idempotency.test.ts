// E2E for the idempotency:guard component, run in-process via jco (in-memory kv
// shim). Covers first-call reserve, concurrent-duplicate in-progress, replay of
// a completed result, and forget.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { store as idem } from "../gen/idempotency_guard.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("idempotency:guard component", () => {
  it("first begin reserves the key (returns none)", () => {
    assert.equal(idem.begin("order-1", 3600n), undefined);
  });

  it("a second begin before complete is an in-progress duplicate", () => {
    idem.begin("order-2", 3600n); // reserve
    assert.throws(
      () => idem.begin("order-2", 3600n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "in-progress",
    );
  });

  it("after complete, begin replays the stored response", () => {
    idem.begin("order-3", 3600n);
    idem.complete("order-3", 201, enc('{"id":"abc"}'));
    const replay = idem.begin("order-3", 3600n);
    assert.ok(replay);
    assert.equal(replay.status, 201);
    assert.equal(dec(replay.body), '{"id":"abc"}');
  });

  it("forget reclaims a key for retry", () => {
    idem.begin("order-4", 3600n);
    idem.forget("order-4");
    // reservation gone -> begin is first-caller again.
    assert.equal(idem.begin("order-4", 3600n), undefined);
  });
});
