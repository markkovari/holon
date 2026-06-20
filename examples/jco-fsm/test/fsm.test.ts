// E2E for the fsm:workflow component, run in-process via jco (in-memory kv
// shim). Models the vet-clinic appointment lifecycle as a declarative state
// machine: define the states + legal transitions once, then drive instances
// through them. Every fire is validated against the definition; an append-only
// history records each accepted transition.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { engine as fsm } from "../gen/fsm_workflow.js";

// The appointment lifecycle.
const APPOINTMENT = {
  states: ["booked", "confirmed", "completed", "cancelled"],
  initial: "booked",
  transitions: [
    { event: "confirm", source: "booked", target: "confirmed" },
    { event: "complete", source: "confirmed", target: "completed" },
    { event: "cancel", source: "booked", target: "cancelled" },
    { event: "cancel", source: "confirmed", target: "cancelled" },
  ],
  terminal: ["completed", "cancelled"],
};

const tag = (t: string) => (e: { payload?: { tag: string } }) =>
  e?.payload?.tag === t;

describe("fsm:workflow component", () => {
  it("defines a machine and reads its definition back", () => {
    fsm.define("appointment", APPOINTMENT);
    const def = fsm.getDefinition("appointment");
    assert.equal(def.states.length, 4);
    assert.equal(def.initial, "booked");
  });

  it("rejects a definition whose transition targets an unknown state", () => {
    assert.throws(
      () =>
        fsm.define("bad", {
          states: ["a", "b"],
          initial: "a",
          transitions: [{ event: "go", source: "a", target: "nope" }],
          terminal: [],
        }),
      tag("invalid-definition"),
    );
  });

  it("createInstance starts in the initial state", () => {
    const s = fsm.createInstance("appointment", "appt-1");
    assert.equal(s.state, "booked");
    assert.equal(s.done, false);
    assert.equal(s.steps, 0);
  });

  it("allowedEvents lists only the legal events for the current state", () => {
    const events = fsm.allowedEvents("appointment", "appt-1");
    assert.ok(events.includes("confirm"));
    assert.ok(events.includes("cancel"));
    assert.ok(!events.includes("complete"));
  });

  it("canFire reflects whether an event is legal now", () => {
    assert.equal(fsm.canFire("appointment", "appt-1", "complete"), false);
    assert.equal(fsm.canFire("appointment", "appt-1", "confirm"), true);
  });

  it("fire advances the state and bumps the step count", () => {
    const a = fsm.fire("appointment", "appt-1", "confirm");
    assert.equal(a.state, "confirmed");
    assert.equal(a.steps, 1);

    const b = fsm.fire("appointment", "appt-1", "complete");
    assert.equal(b.state, "completed");
    assert.equal(b.done, true);
    assert.equal(b.steps, 2);
  });

  it("firing on a terminal instance is an illegal transition (carries current state)", () => {
    assert.throws(
      () => fsm.fire("appointment", "appt-1", "confirm"),
      (e: { payload?: { tag: string; val?: string } }) =>
        e?.payload?.tag === "illegal-transition" &&
        e?.payload?.val === "completed",
    );
  });

  it("a fresh instance can be cancelled straight from booked", () => {
    fsm.createInstance("appointment", "appt-2");
    const s = fsm.fire("appointment", "appt-2", "cancel");
    assert.equal(s.state, "cancelled");
    assert.equal(s.done, true);
  });

  it("history is append-only, oldest first", () => {
    const h = fsm.history("appointment", "appt-1");
    assert.equal(h.length, 2);

    assert.equal(h[0].event, "confirm");
    assert.equal(h[0].source, "booked");
    assert.equal(h[0].target, "confirmed");

    assert.equal(h[1].event, "complete");
    assert.equal(h[1].source, "confirmed");
    assert.equal(h[1].target, "completed");

    // Every entry is timestamped (bigint; may be 0n under the shim's clock).
    assert.equal(typeof h[0].at, "bigint");
  });

  it("rejects unknown instances and unknown machines", () => {
    assert.throws(
      () => fsm.getStatus("appointment", "no-such-instance"),
      tag("unknown-instance"),
    );
    assert.throws(
      () => fsm.createInstance("no-such-machine", "x"),
      tag("unknown-machine"),
    );
  });
});
