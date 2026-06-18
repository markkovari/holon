// E2E for the validate:schema component, run in-process via jco (pure-compute,
// no shims). Exercises declarative validation: required/type/min-len/format/
// range/enum checks plus uuid + alphanumeric kinds and non-object input.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { validator as validate } from "../gen/validate.js";
import type { Rule, FieldError } from "../gen/validate.js";

// Helper to build a fully-populated Rule. WIT requires every field present:
// minLen/maxLen default to 0 (unused), oneOf to [] (unused). minValue/maxValue
// are WIT option<f64> -> pass `undefined` to omit (none), or a number for some.
function rule(field: string, kind: Rule["kind"], extra: Partial<Rule> = {}): Rule {
  return {
    field,
    kind,
    required: false,
    minLen: 0,
    maxLen: 0,
    minValue: undefined,
    maxValue: undefined,
    oneOf: [],
    ...extra,
  };
}

const codes = (errs: FieldError[]) => errs.map((e) => e.code);

const personRules: Rule[] = [
  rule("name", "text", { required: true, minLen: 2, maxLen: 50 }),
  rule("age", "integer", { required: true, minValue: 0, maxValue: 130 }),
  rule("email", "email", { required: true }),
  rule("role", "text", { oneOf: ["admin", "user"] }),
];

describe("validate:schema component", () => {
  it("a valid object passes with no errors", () => {
    const json = JSON.stringify({ name: "Al", age: 30, email: "a@b.com", role: "user" });
    assert.deepEqual(validate.validate(json, personRules), []);
  });

  it("missing required field -> code 'required'", () => {
    const json = JSON.stringify({ age: 30, email: "a@b.com", role: "user" });
    const errs = validate.validate(json, personRules);
    assert.equal(errs.length, 1);
    assert.equal(errs[0].field, "name");
    assert.equal(errs[0].code, "required");
  });

  it("wrong type -> code 'type'", () => {
    const json = JSON.stringify({ name: "Alice", age: "x", email: "a@b.com", role: "user" });
    const errs = validate.validate(json, personRules);
    assert.ok(codes(errs).includes("type"));
    assert.equal(errs.find((e) => e.code === "type")?.field, "age");
  });

  it("value too short -> code 'min-len'", () => {
    const json = JSON.stringify({ name: "A", age: 30, email: "a@b.com", role: "user" });
    const errs = validate.validate(json, personRules);
    assert.ok(codes(errs).includes("min-len"));
    assert.equal(errs.find((e) => e.code === "min-len")?.field, "name");
  });

  it("bad email -> code 'format'", () => {
    const json = JSON.stringify({ name: "Alice", age: 30, email: "nope", role: "user" });
    const errs = validate.validate(json, personRules);
    assert.ok(codes(errs).includes("format"));
    assert.equal(errs.find((e) => e.code === "format")?.field, "email");
  });

  it("value out of range -> code 'max-value'", () => {
    const json = JSON.stringify({ name: "Alice", age: 200, email: "a@b.com", role: "user" });
    const errs = validate.validate(json, personRules);
    assert.ok(codes(errs).includes("max-value"));
    assert.equal(errs.find((e) => e.code === "max-value")?.field, "age");
  });

  it("value not in enum -> code 'one-of'", () => {
    const json = JSON.stringify({ name: "Alice", age: 30, email: "a@b.com", role: "root" });
    const errs = validate.validate(json, personRules);
    assert.ok(codes(errs).includes("one-of"));
    assert.equal(errs.find((e) => e.code === "one-of")?.field, "role");
  });

  it("non-object JSON -> single 'format' error on field ''", () => {
    for (const json of ["[]", "5"]) {
      const errs = validate.validate(json, personRules);
      assert.equal(errs.length, 1);
      assert.equal(errs[0].field, "");
      assert.equal(errs[0].code, "format");
    }
  });

  it("uuid kind accepts a valid uuid and rejects garbage", () => {
    const rules: Rule[] = [rule("id", "uuid", { required: true })];
    const ok = validate.validate(
      JSON.stringify({ id: "123e4567-e89b-12d3-a456-426614174000" }),
      rules,
    );
    assert.deepEqual(ok, []);

    const bad = validate.validate(JSON.stringify({ id: "not-a-uuid" }), rules);
    assert.equal(bad.length, 1);
    assert.equal(bad[0].field, "id");
    assert.equal(bad[0].code, "format");
  });

  it("alphanumeric kind accepts letters/digits and rejects symbols", () => {
    const rules: Rule[] = [rule("token", "alphanumeric", { required: true })];
    const ok = validate.validate(JSON.stringify({ token: "abc123" }), rules);
    assert.deepEqual(ok, []);

    const bad = validate.validate(JSON.stringify({ token: "abc 123!" }), rules);
    assert.equal(bad.length, 1);
    assert.equal(bad[0].field, "token");
    assert.equal(bad[0].code, "format");
  });
});
