// E2E for the outbox:dispatch component, run in-process via jco (in-memory kv +
// atomics + config shims). Covers enqueue -> claim (lease flips state to
// in-flight) -> ack (delivered, gone) and the fail/dead-letter/replay surface.
// Assertions are kept robust to timing: enqueue with delaySeconds 0n so events
// are immediately claimable, claim with a long leaseSeconds so a second claim
// sees them leased.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { queue as outbox } from "../gen/outbox.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("outbox:dispatch component", () => {
  it("enqueue returns an id", () => {
    const id = outbox.enqueue("orders.created", enc('{"order":1}'), 0n);
    assert.equal(typeof id, "string");
    assert.ok(id.length > 0);
  });

  it("claim returns the event and flips it to in-flight", () => {
    const id = outbox.enqueue("orders.shipped", enc("ship-1"), 0n);
    const batch = outbox.claim(10, 60n);
    const ev = batch.find((e) => e.id === id);
    assert.ok(ev, "enqueued event should be claimable");
    assert.equal(ev.topic, "orders.shipped");
    assert.equal(dec(ev.payload), "ship-1");
    assert.equal(ev.state, "in-flight");
  });

  it("a second immediate claim does not re-return leased events", () => {
    const id = outbox.enqueue("orders.paid", enc("pay-1"), 0n);
    const first = outbox.claim(10, 60n);
    assert.ok(first.some((e) => e.id === id), "first claim leases the event");
    const second = outbox.claim(10, 60n);
    assert.ok(
      !second.some((e) => e.id === id),
      "leased event must not reappear in an immediate second claim",
    );
  });

  it("ack removes the event; acking again throws not-found", () => {
    const id = outbox.enqueue("orders.refunded", enc("refund-1"), 0n);
    outbox.claim(10, 60n); // lease it
    outbox.ack(id); // delivered, gone
    assert.throws(
      () => outbox.ack(id),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("fail returns a valid state string", () => {
    const id = outbox.enqueue("orders.cancelled", enc("cancel-1"), 0n);
    outbox.claim(10, 60n); // lease it
    const state = outbox.fail(id);
    assert.equal(typeof state, "string");
    assert.ok(["pending", "in-flight", "dead"].includes(state));
  });

  it("deadLetters and replay are callable", () => {
    const dead = outbox.deadLetters(10);
    assert.ok(Array.isArray(dead));
    // replay of an unknown id should not blow up the harness beyond a typed
    // host error; we only assert the surface is reachable.
    assert.doesNotThrow(() => {
      try {
        outbox.replay("does-not-exist");
      } catch (e) {
        // a typed not-found is acceptable; rethrow anything unexpected.
        if ((e as { payload?: { tag: string } })?.payload?.tag !== "not-found") {
          throw e;
        }
      }
    });
  });
});
