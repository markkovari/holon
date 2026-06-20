// E2E for the policy:guard component, run in-process via jco (in-memory kv
// shim). Models the vet-clinic appointment authorization rules: declarative,
// attribute/row-level (ABAC). Default-deny, deny-overrides at equal priority.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { guard as policy } from "../gen/policy_guard.js";

type Attr = { key: string; value: string };
type Condition = { left: string; op: string; right: string };
type Rule = {
  id: string;
  action: string;
  effect: string;
  conditions: Condition[];
  priority: number;
};

const RULES: Rule[] = [
  // an owner may cancel their OWN appointment
  {
    id: "owner-cancel",
    action: "cancel",
    effect: "allow",
    priority: 10,
    conditions: [{ left: "resource.owner", op: "eq", right: "principal.subject" }],
  },
  // doctors + admins may do anything (role in a list)
  {
    id: "staff-any",
    action: "*",
    effect: "allow",
    priority: 10,
    conditions: [{ left: "principal.role", op: "in-list", right: "doctor,admin" }],
  },
  // explicitly deny cancel on a completed appointment (deny overrides at same priority)
  {
    id: "no-cancel-done",
    action: "cancel",
    effect: "deny",
    priority: 10,
    conditions: [{ left: "resource.status", op: "eq", right: "completed" }],
  },
];

describe("policy:guard component", () => {
  it("setRules + getRules round-trips the 3 rules", () => {
    policy.setRules("appointments", RULES);
    assert.equal(policy.getRules("appointments").length, 3);
  });

  it("an owner may cancel their own booked appointment", () => {
    const r = policy.can(
      "appointments",
      "cancel",
      [{ key: "subject", value: "u1" }],
      [
        { key: "owner", value: "u1" },
        { key: "status", value: "booked" },
      ],
    );
    assert.equal(r.allowed, true);
    assert.equal(r.ruleId, "owner-cancel");
  });

  it("a non-owner cancelling someone else's appointment is denied (default deny)", () => {
    const r = policy.can(
      "appointments",
      "cancel",
      [{ key: "subject", value: "u2" }],
      [
        { key: "owner", value: "u1" },
        { key: "status", value: "booked" },
      ],
    );
    assert.equal(r.allowed, false);
    assert.equal(r.ruleId, "");
  });

  it("deny overrides allow at equal priority (owner cannot cancel a completed appointment)", () => {
    const r = policy.can(
      "appointments",
      "cancel",
      [{ key: "subject", value: "u1" }],
      [
        { key: "owner", value: "u1" },
        { key: "status", value: "completed" },
      ],
    );
    assert.equal(r.allowed, false);
    assert.equal(r.ruleId, "no-cancel-done");
  });

  it("staff wildcard: a doctor may confirm; a pet-owner role may not", () => {
    const doc = policy.can(
      "appointments",
      "confirm",
      [{ key: "role", value: "doctor" }],
      [],
    );
    assert.equal(doc.allowed, true);
    assert.equal(doc.ruleId, "staff-any");

    const owner = policy.can(
      "appointments",
      "confirm",
      [{ key: "role", value: "pet-owner" }],
      [],
    );
    assert.equal(owner.allowed, false);
  });

  it("enforce returns the boolean form of can", () => {
    assert.equal(
      policy.enforce(
        "appointments",
        "cancel",
        [{ key: "subject", value: "u1" }],
        [
          { key: "owner", value: "u1" },
          { key: "status", value: "booked" },
        ],
      ),
      true,
    );
    assert.equal(
      policy.enforce(
        "appointments",
        "cancel",
        [{ key: "subject", value: "u2" }],
        [{ key: "owner", value: "u1" }],
      ),
      false,
    );
  });

  it("setRules rejects a structurally invalid rule", () => {
    assert.throws(
      () =>
        policy.setRules("x", [
          {
            id: "bad",
            action: "a",
            effect: "allow",
            priority: 1,
            conditions: [{ left: "", op: "eq", right: "y" }],
          },
        ]),
      (e: { payload?: unknown }) =>
        (typeof e?.payload === "string" && e.payload === "invalid-rule") ||
        (typeof e?.payload === "object" &&
          e?.payload !== null &&
          (e.payload as { tag?: string }).tag === "invalid-rule"),
    );
  });
});
