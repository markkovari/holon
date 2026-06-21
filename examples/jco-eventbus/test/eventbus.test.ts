// E2E for the event:bus component, run in-process via jco. Covers publish ->
// poll (per-group offset) -> ack (advances), fan-out (two groups each see every
// event independently), pending backlog, a new group reading from the start of
// the log, and at-least-once (unacked events re-poll).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { bus } from "../gen/eventbus.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("event:bus component", () => {
  it("publish returns a monotonic id; poll returns it oldest-first", () => {
    const id1 = bus.publish("appt.booked", enc("a1"));
    const id2 = bus.publish("appt.booked", enc("a2"));
    assert.ok(Number(id2) > Number(id1), "ids are monotonic per topic");
    const evs = bus.poll("appt.booked", "notifier", 10);
    assert.equal(evs.length, 2);
    assert.equal(dec(evs[0].payload), "a1");
    assert.equal(dec(evs[1].payload), "a2");
  });

  it("ack advances the group offset; a later poll skips acked events", () => {
    bus.publish("orders", enc("o1"));
    bus.publish("orders", enc("o2"));
    const first = bus.poll("orders", "billing", 10);
    assert.equal(first.length, 2);
    bus.ack("orders", "billing", [first[0].id]); // ack only the first
    const second = bus.poll("orders", "billing", 10);
    assert.equal(second.length, 1, "only the unacked event remains");
    assert.equal(dec(second[0].payload), "o2");
  });

  it("two groups consume the same topic independently (fan-out)", () => {
    bus.publish("user.created", enc("u1"));
    bus.publish("user.created", enc("u2"));
    const a = bus.poll("user.created", "audit", 10);
    const s = bus.poll("user.created", "search", 10);
    assert.equal(a.length, 2, "audit sees both");
    assert.equal(s.length, 2, "search sees both — groups do not steal");
    // audit acks everything; search still has its full backlog
    bus.ack("user.created", "audit", a.map((e) => e.id));
    assert.equal(bus.poll("user.created", "audit", 10).length, 0, "audit drained");
    assert.equal(bus.poll("user.created", "search", 10).length, 2, "search untouched");
  });

  it("pending reports a group's unacked backlog", () => {
    bus.publish("metrics", enc("m1"));
    bus.publish("metrics", enc("m2"));
    bus.publish("metrics", enc("m3"));
    assert.equal(bus.pending("metrics", "sink"), 3n);
    const evs = bus.poll("metrics", "sink", 10);
    bus.ack("metrics", "sink", evs.map((e) => e.id));
    assert.equal(bus.pending("metrics", "sink"), 0n, "drained to zero");
  });

  it("a brand-new group reads the whole log from the beginning", () => {
    bus.publish("history", enc("h1"));
    bus.publish("history", enc("h2"));
    // 'latecomer' never polled before, yet sees all prior events.
    const evs = bus.poll("history", "latecomer", 10);
    assert.equal(evs.length, 2, "new group starts at offset 0");
  });

  it("unacked events re-poll (at-least-once)", () => {
    bus.publish("retry", enc("r1"));
    const a = bus.poll("retry", "worker", 10);
    assert.equal(a.length, 1);
    // worker crashed before ack -> the same event is still visible
    const b = bus.poll("retry", "worker", 10);
    assert.equal(b.length, 1, "unacked event re-delivered");
    assert.equal(b[0].id, a[0].id);
  });

  it("topics lists every topic that has a log", () => {
    bus.publish("topic.x", enc("x"));
    const ts = bus.topics();
    assert.ok(ts.includes("topic.x"), "published topic appears");
  });
});
