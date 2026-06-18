// E2E for the json:patch component, run in-process via jco. Pure compute (no
// host shims): RFC 6902 JSON Patch (applyPatch), RFC 7386 merge-patch
// (applyMerge), and a merge-patch diff. Results are JSON strings, so we compare
// by JSON.parse deep-equal rather than raw text (key order is not guaranteed).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { patcher as jp } from "../gen/jsonpatch.js";

const tagOf = (e: { payload?: { tag: string } }) => e?.payload?.tag;

describe("json:patch RFC 6902 (applyPatch)", () => {
  it("add", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"a":1}', '[{"op":"add","path":"/b","value":2}]')),
      { a: 1, b: 2 },
    );
  });

  it("remove", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"a":1,"b":2}', '[{"op":"remove","path":"/b"}]')),
      { a: 1 },
    );
  });

  it("replace", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"a":1}', '[{"op":"replace","path":"/a","value":9}]')),
      { a: 9 },
    );
  });

  it("array append with '-'", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"l":[1,2]}', '[{"op":"add","path":"/l/-","value":3}]')),
      { l: [1, 2, 3] },
    );
  });

  it("move", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"a":1}', '[{"op":"move","from":"/a","path":"/b"}]')),
      { b: 1 },
    );
  });

  it("copy", () => {
    assert.deepEqual(
      JSON.parse(jp.applyPatch('{"a":1}', '[{"op":"copy","from":"/a","path":"/b"}]')),
      { a: 1, b: 1 },
    );
  });

  it("test passes then replace applies", () => {
    assert.deepEqual(
      JSON.parse(
        jp.applyPatch(
          '{"a":1}',
          '[{"op":"test","path":"/a","value":1},{"op":"replace","path":"/a","value":2}]',
        ),
      ),
      { a: 2 },
    );
  });

  it("test failure throws 'test-failed'", () => {
    assert.throws(
      () => jp.applyPatch('{"a":1}', '[{"op":"test","path":"/a","value":99}]'),
      (e: { payload?: { tag: string } }) => tagOf(e) === "test-failed",
    );
  });

  it("missing target path throws 'path-not-found'", () => {
    assert.throws(
      () => jp.applyPatch('{"a":1}', '[{"op":"replace","path":"/x","value":1}]'),
      (e: { payload?: { tag: string } }) => tagOf(e) === "path-not-found",
    );
  });

  it("bogus op throws 'invalid-patch'", () => {
    assert.throws(
      () => jp.applyPatch('{"a":1}', '[{"op":"bogus","path":"/a"}]'),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-patch",
    );
  });

  it("malformed document throws 'invalid-json'", () => {
    assert.throws(
      () => jp.applyPatch("not json", "[]"),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-json",
    );
  });
});

describe("json:patch RFC 7386 (applyMerge)", () => {
  it("null deletes a key, others add/replace", () => {
    assert.deepEqual(
      JSON.parse(jp.applyMerge('{"a":1,"b":2}', '{"b":null,"c":3}')),
      { a: 1, c: 3 },
    );
  });

  it("merges nested objects", () => {
    assert.deepEqual(
      JSON.parse(jp.applyMerge('{"o":{"x":1,"y":2}}', '{"o":{"y":3}}')),
      { o: { x: 1, y: 3 } },
    );
  });
});

describe("json:patch diff (merge-patch)", () => {
  it("round-trips: applyMerge(source, diff(source,target)) === target", () => {
    const source = '{"a":1,"b":2}';
    const target = '{"a":1,"b":9,"c":3}';
    const mergePatch = jp.diff(source, target);
    assert.deepEqual(
      JSON.parse(jp.applyMerge(source, mergePatch)),
      JSON.parse(target),
    );
  });

  it("diff emits deletions for removed keys", () => {
    const source = '{"a":1,"x":2}';
    const target = '{"a":1}';
    const mergePatch = jp.diff(source, target);
    assert.deepEqual(JSON.parse(jp.applyMerge(source, mergePatch)), { a: 1 });
  });
});
